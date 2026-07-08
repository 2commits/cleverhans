//! Dogfood server + eval runner for the CleverHans demo registry.
//!
//! ```text
//! ANTHROPIC_API_KEY=... cargo run -p cleverhans-demo -- serve
//! OLLAMA_MODEL=qwen3    cargo run -p cleverhans-demo -- serve
//! ANTHROPIC_API_KEY=... cargo run -p cleverhans-demo -- eval crates/cleverhans-demo/eval-cases.json
//! ```
//!
//! `serve` hosts the chat page on <http://127.0.0.1:8787> and the envelope
//! stream at `/agent`. `eval` runs the action-mapping suite and exits
//! non-zero if any case fails.

use cleverhans_demo::registry;

use std::sync::Arc;

use anyhow::{Context as _, bail};
use axum::Router;
use axum::http::HeaderMap;
use axum::response::Html;
use axum::routing::get;

use cleverhans::prelude::*;

use registry::{AllowAll, DemoUser, Store, build_registry, context_resolver};

fn agent() -> anyhow::Result<Arc<Agent<DemoUser>>> {
    let store = Store::seeded();
    let config = AgentConfig {
        app_instructions: Some(
            "This app is a document workspace: the selected record is always a \
             document, and document tools act on it. You cannot navigate or \
             open documents yourself — when nothing is selected, tell the user \
             what to open, and once they have navigated, call the tool again."
                .to_owned(),
        ),
        ..AgentConfig::default()
    };
    Ok(Arc::new(Agent::with_config(
        Arc::new(build_registry(&store)),
        // OLLAMA_MODEL wins, then ANTHROPIC_API_KEY (+ optional
        // ANTHROPIC_MODEL, e.g. claude-haiku-4-5 for a cheaper run).
        cleverhans::llm::from_env()?,
        Arc::new(AllowAll),
        Arc::new(context_resolver()),
        config,
    )))
}

/// Demo-only: every connection is the same demo user. A real app maps the
/// session cookie / bearer token here (spec §10).
struct EveryoneIsDemo;

impl PrincipalExtractor<DemoUser> for EveryoneIsDemo {
    fn extract(&self, _headers: &HeaderMap) -> Result<DemoUser, axum::http::StatusCode> {
        Ok(DemoUser {
            name: "demo".to_owned(),
        })
    }
}

async fn serve() -> anyhow::Result<()> {
    let app = Router::new()
        .route("/", get(|| async { Html(include_str!("chat.html")) }))
        .merge(agent_router("/agent", agent()?, Arc::new(EveryoneIsDemo)));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:8787")
        .await
        .context("bind 127.0.0.1:8787")?;
    eprintln!("chat: http://127.0.0.1:8787  (envelope stream at /agent)");
    axum::serve(listener, app).await.context("serve")
}

async fn eval(path: &str) -> anyhow::Result<()> {
    let json = std::fs::read_to_string(path).with_context(|| format!("read {path}"))?;
    let cases = cleverhans::evals::load_cases(&json).context("parse cases")?;
    let agent = agent()?;
    let principal = DemoUser {
        name: "eval".to_owned(),
    };
    eprintln!("running {} case(s) as `{}`", cases.len(), principal.name);
    let report = cleverhans::evals::run_suite(&agent, &principal, cases).await;
    print!("{report}");
    if !report.all_passed() {
        bail!("eval suite failed");
    }
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // RUST_LOG overrides; default surfaces the WS envelope traffic.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,cleverhans_ws=info".into()),
        )
        .init();
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("serve") => serve().await,
        Some("eval") => {
            let path = args
                .get(2)
                .context("usage: cleverhans-demo eval <cases.json>")?;
            eval(path).await
        }
        _ => bail!("usage: cleverhans-demo <serve | eval cases.json>"),
    }
}
