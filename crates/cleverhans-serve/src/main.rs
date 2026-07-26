//! The `cleverhans` binary: `serve` hosts the standalone agent service
//! (spec §10.2) from a registry document and a `cleverhans.toml`;
//! `host-check` replays the §14 host conformance vectors against a
//! candidate host; `mock-host` runs the known-good reference host for
//! integration tests.

use std::path::PathBuf;

use anyhow::Context as _;
use clap::{Parser, Subcommand};

use cleverhans_conformance::fixture::AuthzScript;
use cleverhans_conformance::mock_host::HostScript;
use cleverhans_conformance::{
    Fixture, HostCheckOutcome, HostCheckTarget, HostVector, MockHost, run_host_vector,
};
use cleverhans_serve::config::Config;
use cleverhans_serve::{build_app, load_schema};

/// The fixture the mock host serves and host-check assumes (a copy of
/// `spec/vectors/fixtures/co-buyer.json`; a test keeps them identical).
const FIXTURE: &str = include_str!("../embedded/co-buyer.json");

/// Embedded copies of `spec/vectors/webhook/host/` (same sync test).
const HOST_VECTORS: &[(&str, &str)] = &[
    (
        "authorize_allow",
        include_str!("../embedded/authorize_allow.json"),
    ),
    (
        "dry_run_preview",
        include_str!("../embedded/dry_run_preview.json"),
    ),
    (
        "execute_executed",
        include_str!("../embedded/execute_executed.json"),
    ),
    (
        "execute_idempotent_replay",
        include_str!("../embedded/execute_idempotent_replay.json"),
    ),
    (
        "missing_secret_rejected",
        include_str!("../embedded/missing_secret_rejected.json"),
    ),
    (
        "unknown_version_rejected",
        include_str!("../embedded/unknown_version_rejected.json"),
    ),
    (
        "verify_session_returns_principal",
        include_str!("../embedded/verify_session_returns_principal.json"),
    ),
    (
        "build_slots_returns_slots",
        include_str!("../embedded/build_slots_returns_slots.json"),
    ),
];

#[derive(Parser)]
#[command(
    name = "cleverhans",
    version,
    about = "Standalone CleverHans agent service (propose-only HITL, spec §10.2)"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Host the agent service from a registry document and a config file.
    Serve {
        /// Path to the registry document (spec §4).
        #[arg(long)]
        registry: PathBuf,
        /// Path to cleverhans.toml.
        #[arg(long)]
        config: PathBuf,
        /// Override [server].bind.
        #[arg(long)]
        bind: Option<String>,
    },
    /// Replay the §14 host conformance vectors against a candidate host.
    HostCheck {
        /// Host origin, e.g. https://your-app.
        #[arg(long)]
        base_url: String,
        /// The service secret the host expects.
        #[arg(long)]
        secret: String,
        /// §14.2 HMAC signing key, for hosts that require signatures.
        #[arg(long)]
        signing_key: Option<String>,
    },
    /// Run the known-good reference host (co-buyer fixture semantics).
    MockHost {
        /// Listen address.
        #[arg(long, default_value = "127.0.0.1:8790")]
        bind: String,
        /// The bearer secret the mock host requires.
        #[arg(long, default_value = "dev-secret")]
        secret: String,
        /// Serve a custom conformance fixture (registry + seam scripts,
        /// `spec/vectors/README.md` format) instead of the embedded
        /// co-buyer demo — e.g. your own registry with scripted handlers.
        #[arg(long)]
        fixture: Option<PathBuf>,
        /// Require a valid §14.2 signature on every delivery (HMAC key).
        #[arg(long)]
        signing_key: Option<String>,
    },
}

fn init_tracing(metrics_layer: Option<cleverhans_serve::telemetry::MetricsLayer>) {
    use tracing_subscriber::EnvFilter;
    use tracing_subscriber::layer::SubscriberExt as _;
    use tracing_subscriber::util::SubscriberInitExt as _;
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with(tracing_subscriber::fmt::layer())
        .with(metrics_layer)
        .init();
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    match Cli::parse().command {
        Command::Serve {
            registry,
            config,
            bind,
        } => serve(&registry, &config, bind).await,
        Command::HostCheck {
            base_url,
            secret,
            signing_key,
        } => {
            init_tracing(None);
            host_check(&base_url, &secret, signing_key.as_deref()).await
        }
        Command::MockHost {
            bind,
            secret,
            fixture,
            signing_key,
        } => {
            init_tracing(None);
            mock_host(&bind, &secret, fixture.as_deref(), signing_key.as_deref()).await
        }
    }
}

async fn serve(registry: &PathBuf, config: &PathBuf, bind: Option<String>) -> anyhow::Result<()> {
    let registry_json = std::fs::read_to_string(registry)
        .with_context(|| format!("read registry {}", registry.display()))?;
    let schema = load_schema(&registry_json).map_err(anyhow::Error::msg)?;
    let config_text = std::fs::read_to_string(config)
        .with_context(|| format!("read config {}", config.display()))?;
    let parsed = Config::from_toml(&config_text)?;
    // Telemetry before the subscriber: the metrics layer converts the lib
    // crates' telemetry events; the guard flushes on shutdown.
    let telemetry =
        cleverhans_serve::telemetry::init(&parsed.telemetry).map_err(anyhow::Error::msg)?;
    let (_telemetry_guard, metrics_layer) = match telemetry {
        Some((guard, layer)) => (Some(guard), Some(layer)),
        None => (None, None),
    };
    init_tracing(metrics_layer);
    let mut resolved = parsed.resolve(&schema)?;
    if let Some(bind) = bind {
        resolved.bind = bind;
    }
    let llm = cleverhans::llm::build_llm(parsed.llm.resolve()?);
    let app = build_app(&resolved, &schema, llm)?;

    let listener = tokio::net::TcpListener::bind(&resolved.bind)
        .await
        .with_context(|| format!("bind {}", resolved.bind))?;
    tracing::info!(
        bind = resolved.bind.as_str(),
        path = resolved.path.as_str(),
        upstream = parsed.upstream.base_url.as_str(),
        "cleverhans service up"
    );
    axum::serve(listener, app).await?;
    Ok(())
}

async fn host_check(base_url: &str, secret: &str, signing_key: Option<&str>) -> anyhow::Result<()> {
    let mut target = HostCheckTarget::new(base_url, secret);
    target.signing_key = signing_key.map(str::to_owned);
    let mut failures = 0usize;
    for (name, json) in HOST_VECTORS {
        let vector: HostVector =
            serde_json::from_str(json).with_context(|| format!("parse vector `{name}`"))?;
        match run_host_vector(&target, &vector).await {
            Ok(HostCheckOutcome::Passed) => println!("[PASS] {name}"),
            Ok(HostCheckOutcome::Skipped(reason)) => println!("[SKIP] {name}: {reason}"),
            Err(err) => {
                failures += 1;
                println!("[FAIL] {name}: {err}");
            }
        }
    }
    if failures > 0 {
        anyhow::bail!(
            "{failures}/{} host vectors failed — the host is not §14-conformant",
            HOST_VECTORS.len()
        );
    }
    println!("host is §14-conformant ({} vectors)", HOST_VECTORS.len());
    Ok(())
}

async fn mock_host(
    bind: &str,
    secret: &str,
    fixture: Option<&std::path::Path>,
    signing_key: Option<&str>,
) -> anyhow::Result<()> {
    let (fixture, source): (Fixture, String) = match fixture {
        Some(path) => {
            let json = std::fs::read_to_string(path)
                .with_context(|| format!("read fixture {}", path.display()))?;
            (
                serde_json::from_str(&json)
                    .with_context(|| format!("parse fixture {}", path.display()))?,
                path.display().to_string(),
            )
        }
        None => (
            serde_json::from_str(FIXTURE).context("parse embedded fixture")?,
            "embedded co-buyer".to_owned(),
        ),
    };
    let host = MockHost::spawn_at(
        fixture,
        AuthzScript::default(),
        HostScript::new(),
        secret,
        signing_key,
        bind,
    )
    .await;
    tracing::info!(
        addr = %host.addr,
        secret,
        fixture = source.as_str(),
        "mock host up — endpoints: /cleverhans/{{verify_session,authorize,dry_run,execute}}"
    );
    tokio::signal::ctrl_c().await?;
    Ok(())
}
