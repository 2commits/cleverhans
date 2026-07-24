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
use cleverhans_conformance::{Fixture, HostCheckTarget, HostVector, MockHost, run_host_vector};
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
    },
    /// Run the known-good reference host (co-buyer fixture semantics).
    MockHost {
        /// Listen address.
        #[arg(long, default_value = "127.0.0.1:8790")]
        bind: String,
        /// The bearer secret the mock host requires.
        #[arg(long, default_value = "dev-secret")]
        secret: String,
    },
}

fn init_tracing() {
    use tracing_subscriber::EnvFilter;
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();
    match Cli::parse().command {
        Command::Serve {
            registry,
            config,
            bind,
        } => serve(&registry, &config, bind).await,
        Command::HostCheck { base_url, secret } => host_check(&base_url, &secret).await,
        Command::MockHost { bind, secret } => mock_host(&bind, &secret).await,
    }
}

async fn serve(registry: &PathBuf, config: &PathBuf, bind: Option<String>) -> anyhow::Result<()> {
    let registry_json = std::fs::read_to_string(registry)
        .with_context(|| format!("read registry {}", registry.display()))?;
    let schema = load_schema(&registry_json).map_err(anyhow::Error::msg)?;
    let config_text = std::fs::read_to_string(config)
        .with_context(|| format!("read config {}", config.display()))?;
    let parsed = Config::from_toml(&config_text)?;
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

async fn host_check(base_url: &str, secret: &str) -> anyhow::Result<()> {
    let target = HostCheckTarget::new(base_url, secret);
    let mut failures = 0usize;
    for (name, json) in HOST_VECTORS {
        let vector: HostVector =
            serde_json::from_str(json).with_context(|| format!("parse vector `{name}`"))?;
        match run_host_vector(&target, &vector).await {
            Ok(()) => println!("[PASS] {name}"),
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

async fn mock_host(bind: &str, secret: &str) -> anyhow::Result<()> {
    let fixture: Fixture = serde_json::from_str(FIXTURE).context("parse embedded fixture")?;
    let host = MockHost::spawn_at(
        fixture,
        AuthzScript::default(),
        HostScript::new(),
        secret,
        bind,
    )
    .await;
    tracing::info!(
        addr = %host.addr,
        secret,
        "mock host up — endpoints: /cleverhans/{{verify_session,authorize,dry_run,execute}}"
    );
    tokio::signal::ctrl_c().await?;
    Ok(())
}
