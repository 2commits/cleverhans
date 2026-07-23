//! `cleverhans.toml` — the deployment configuration of the standalone
//! service. Every struct rejects unknown fields so typos fail at startup,
//! and secrets are only ever named indirectly via `*_env` fields.

use std::collections::BTreeMap;
use std::str::FromStr;
use std::time::Duration;

use serde::Deserialize;

use cleverhans_core::agent::AgentConfig;
use cleverhans_core::declarative::{LlmItem, SlotScript};
use cleverhans_core::schema::RegistrySchema;
use cleverhans_webhook::client::{RetryPolicy, Route, Timeouts};

/// Configuration errors, all fatal at startup.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// TOML syntax or unknown-field error.
    #[error("config: {0}")]
    Parse(#[from] toml::de::Error),
    /// A `"METHOD /path"` string failed to parse.
    #[error("config: {0}")]
    Route(#[from] cleverhans_webhook::client::RouteParseError),
    /// A named env var is unset.
    #[error("config: env var `{0}` (named by `{1}`) is not set")]
    MissingEnv(String, &'static str),
    /// The registry and the actions table disagree.
    #[error("config: {0}")]
    Coverage(String),
    /// The LLM section is unusable.
    #[error("config: llm: {0}")]
    Llm(String),
}

fn default_bind() -> String {
    "127.0.0.1:8789".to_owned()
}

fn default_path() -> String {
    "/agent".to_owned()
}

/// `[server]`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ServerSection {
    /// Listen address.
    pub bind: String,
    /// WS mount path.
    pub path: String,
}

impl Default for ServerSection {
    fn default() -> Self {
        Self {
            bind: default_bind(),
            path: default_path(),
        }
    }
}

/// `[upstream.timeouts]`, milliseconds (spec §14.7 defaults).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TimeoutsSection {
    /// `verify_session` timeout.
    pub verify_ms: Option<u64>,
    /// `authorize` timeout.
    pub authorize_ms: Option<u64>,
    /// `dry_run` timeout.
    pub dry_run_ms: Option<u64>,
    /// `execute` timeout.
    pub execute_ms: Option<u64>,
}

impl TimeoutsSection {
    fn resolve(&self) -> Timeouts {
        let defaults = Timeouts::default();
        let ms = |value: Option<u64>, default: Duration| {
            value.map_or(default, Duration::from_millis)
        };
        Timeouts {
            verify_session: ms(self.verify_ms, defaults.verify_session),
            authorize: ms(self.authorize_ms, defaults.authorize),
            dry_run: ms(self.dry_run_ms, defaults.dry_run),
            execute: ms(self.execute_ms, defaults.execute),
        }
    }
}

/// `[upstream.retry]`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetrySection {
    /// Total execute attempts including the first.
    pub execute_attempts: Option<u32>,
    /// Base backoff between attempts, doubled per retry.
    pub backoff_ms: Option<u64>,
}

impl RetrySection {
    fn resolve(&self) -> RetryPolicy {
        let defaults = RetryPolicy::default();
        RetryPolicy {
            execute_attempts: self.execute_attempts.unwrap_or(defaults.execute_attempts),
            backoff: self
                .backoff_ms
                .map_or(defaults.backoff, Duration::from_millis),
        }
    }
}

/// `[upstream]`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpstreamSection {
    /// Host origin, e.g. `http://localhost:3000`.
    pub base_url: String,
    /// Env var holding the service-to-service secret.
    #[serde(default = "default_secret_env")]
    pub secret_env: String,
    /// Per-endpoint timeouts.
    #[serde(default)]
    pub timeouts: TimeoutsSection,
    /// Execute retry policy.
    #[serde(default)]
    pub retry: RetrySection,
    /// Permit plaintext HTTP to a non-loopback upstream (spec §14.8).
    #[serde(default)]
    pub danger_allow_remote_http: bool,
    /// Permit running without a service secret (spec §14.8).
    #[serde(default)]
    pub danger_allow_missing_secret: bool,
}

fn default_secret_env() -> String {
    "CLEVERHANS_UPSTREAM_SECRET".to_owned()
}

/// `[auth]` — stream-establishment verification (spec §14.3).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthSection {
    /// `"METHOD /path"` of the host's verify_session endpoint.
    pub verify: String,
    /// Header forward-allowlist.
    #[serde(default = "default_forward_headers")]
    pub forward_headers: Vec<String>,
}

fn default_forward_headers() -> Vec<String> {
    vec!["authorization".to_owned(), "cookie".to_owned()]
}

/// `[authz]` — the required authorize endpoint (spec §14.4).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthzSection {
    /// `"METHOD /path"` of the host's authorize endpoint.
    pub endpoint: String,
}

/// `[llm]` — declarative provider selection; secrets via env indirection.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LlmSection {
    /// `anthropic` | `ollama` | `scripted`.
    pub provider: String,
    /// Model override (anthropic/ollama).
    #[serde(default)]
    pub model: Option<String>,
    /// Env var holding the API key (anthropic; default `ANTHROPIC_API_KEY`).
    #[serde(default)]
    pub api_key_env: Option<String>,
    /// Daemon origin (ollama).
    #[serde(default)]
    pub base_url: Option<String>,
    /// Scripted turns (scripted — keyless host-app CI).
    #[serde(default)]
    pub script: Option<Vec<Vec<LlmItem>>>,
}

impl LlmSection {
    /// Resolves into an [`cleverhans::llm::LlmSpec`], reading key env vars.
    ///
    /// # Errors
    ///
    /// [`ConfigError`] on an unknown provider, a missing key env var, or a
    /// scripted provider without a script.
    pub fn resolve(&self) -> Result<cleverhans::llm::LlmSpec, ConfigError> {
        match self.provider.as_str() {
            "anthropic" => {
                let env_name = self.api_key_env.as_deref().unwrap_or("ANTHROPIC_API_KEY");
                let api_key = std::env::var(env_name)
                    .map_err(|_| ConfigError::MissingEnv(env_name.to_owned(), "llm.api_key_env"))?;
                Ok(cleverhans::llm::LlmSpec::Anthropic {
                    api_key,
                    model: self.model.clone(),
                })
            }
            "ollama" => Ok(cleverhans::llm::LlmSpec::Ollama {
                model: self
                    .model
                    .clone()
                    .ok_or_else(|| ConfigError::Llm("ollama requires `model`".to_owned()))?,
                base_url: self.base_url.clone(),
            }),
            "scripted" => Ok(cleverhans::llm::LlmSpec::Scripted {
                script: self
                    .script
                    .clone()
                    .ok_or_else(|| ConfigError::Llm("scripted requires `script`".to_owned()))?,
            }),
            other => Err(ConfigError::Llm(format!(
                "unknown provider `{other}` (anthropic | ollama | scripted)"
            ))),
        }
    }
}

/// `[agent]` — mirrors [`AgentConfig`].
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentSection {
    /// App-specific system-prompt addition.
    pub app_instructions: Option<String>,
    /// Model-fixable validation retry budget.
    pub max_validation_retries: Option<u8>,
    /// Surface a context summary to the model.
    pub describe_context: Option<bool>,
}

impl AgentSection {
    fn resolve(&self) -> AgentConfig {
        let defaults = AgentConfig::default();
        AgentConfig {
            app_instructions: self.app_instructions.clone(),
            max_validation_retries: self
                .max_validation_retries
                .unwrap_or(defaults.max_validation_retries),
            describe_context: self.describe_context.unwrap_or(defaults.describe_context),
        }
    }
}

/// One `[actions."<id>"]` entry (or the `"*"` wildcard template, where
/// `{action}` is substituted with the action ID).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionSection {
    /// `"METHOD /path"` of the execute endpoint.
    #[serde(default)]
    pub execute: Option<String>,
    /// `"METHOD /path"` of the dry_run endpoint.
    #[serde(default)]
    pub dry_run: Option<String>,
    /// Declarative slot table (`const` / `param` / `preview` sources).
    #[serde(default)]
    pub slots: Option<BTreeMap<String, SlotScript>>,
}

/// The whole `cleverhans.toml`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// `[server]`.
    #[serde(default)]
    pub server: ServerSection,
    /// `[upstream]`.
    pub upstream: UpstreamSection,
    /// `[auth]`.
    pub auth: AuthSection,
    /// `[authz]`.
    pub authz: AuthzSection,
    /// `[llm]`.
    pub llm: LlmSection,
    /// `[agent]`.
    #[serde(default)]
    pub agent: AgentSection,
    /// `[actions]`.
    #[serde(default)]
    pub actions: BTreeMap<String, ActionSection>,
}

/// Routes and slots resolved for one action.
#[derive(Debug, Clone)]
pub struct ResolvedAction {
    /// Execute route.
    pub execute: Route,
    /// Dry-run route (present iff the action mutates).
    pub dry_run: Option<Route>,
    /// Declarative slots, if configured.
    pub slots: Option<BTreeMap<String, SlotScript>>,
}

/// Everything `build_app` needs, validated.
#[derive(Debug, Clone)]
pub struct Resolved {
    /// Listen address.
    pub bind: String,
    /// WS mount path.
    pub path: String,
    /// Webhook client configuration.
    pub client: cleverhans_webhook::client::HostClientConfig,
    /// Verify route + forward allowlist.
    pub verify: Route,
    /// See [`AuthSection::forward_headers`].
    pub forward_headers: Vec<String>,
    /// Authorize route.
    pub authorize: Route,
    /// Agent knobs.
    pub agent: AgentConfig,
    /// Per-action routes and slots.
    pub actions: BTreeMap<String, ResolvedAction>,
}

impl Config {
    /// Parses a TOML document.
    ///
    /// # Errors
    ///
    /// [`ConfigError::Parse`] on syntax or unknown fields.
    pub fn from_toml(text: &str) -> Result<Self, ConfigError> {
        Ok(toml::from_str(text)?)
    }

    /// Validates against the registry and resolves routes, secrets, and
    /// slot coverage — every failure names its cause before the service
    /// binds.
    ///
    /// # Errors
    ///
    /// Any [`ConfigError`].
    pub fn resolve(&self, schema: &RegistrySchema) -> Result<Resolved, ConfigError> {
        let secret = match std::env::var(&self.upstream.secret_env) {
            Ok(secret) => Some(secret),
            Err(_) if self.upstream.danger_allow_missing_secret => None,
            Err(_) => {
                return Err(ConfigError::MissingEnv(
                    self.upstream.secret_env.clone(),
                    "upstream.secret_env",
                ));
            }
        };
        let wildcard = self.actions.get("*");
        let mut actions = BTreeMap::new();
        for def in &schema.actions {
            let entry = self.actions.get(&def.id);
            let template = |field: fn(&ActionSection) -> Option<&str>| -> Option<String> {
                entry
                    .and_then(field)
                    .map(str::to_owned)
                    .or_else(|| {
                        wildcard
                            .and_then(field)
                            .map(|route| route.replace("{action}", &def.id))
                    })
            };
            let execute = template(|section| section.execute.as_deref()).ok_or_else(|| {
                ConfigError::Coverage(format!(
                    "action `{}` has no execute route (add [actions.\"{}\"] or an \
                     [actions.\"*\"] wildcard)",
                    def.id, def.id
                ))
            })?;
            let dry_run = template(|section| section.dry_run.as_deref());
            if def.mutates && dry_run.is_none() {
                return Err(ConfigError::Coverage(format!(
                    "action `{}` mutates but has no dry_run route",
                    def.id
                )));
            }
            let slots = entry.and_then(|section| section.slots.clone());
            if let Some(block) = schema.blocks.iter().find(|b| b.block_type == def.block_type) {
                for slot in &block.slots {
                    if slot.required && !slots.as_ref().is_some_and(|s| s.contains_key(&slot.name))
                    {
                        return Err(ConfigError::Coverage(format!(
                            "action `{}`: block `{}` requires slot `{}` — add \
                             [actions.\"{}\".slots]",
                            def.id, def.block_type, slot.name, def.id
                        )));
                    }
                }
            }
            actions.insert(
                def.id.clone(),
                ResolvedAction {
                    execute: Route::from_str(&execute)?,
                    dry_run: dry_run.as_deref().map(Route::from_str).transpose()?,
                    slots,
                },
            );
        }
        for id in self.actions.keys() {
            if id != "*" && !actions.contains_key(id) {
                return Err(ConfigError::Coverage(format!(
                    "[actions.\"{id}\"] matches no action in the registry"
                )));
            }
        }
        Ok(Resolved {
            bind: self.server.bind.clone(),
            path: self.server.path.clone(),
            client: cleverhans_webhook::client::HostClientConfig {
                base_url: self.upstream.base_url.clone(),
                secret,
                timeouts: self.upstream.timeouts.resolve(),
                retry: self.upstream.retry.resolve(),
                danger_allow_remote_http: self.upstream.danger_allow_remote_http,
                danger_allow_missing_secret: self.upstream.danger_allow_missing_secret,
            },
            verify: Route::from_str(&self.auth.verify)?,
            forward_headers: self.auth.forward_headers.clone(),
            authorize: Route::from_str(&self.authz.endpoint)?,
            agent: self.agent.resolve(),
            actions,
        })
    }
}
