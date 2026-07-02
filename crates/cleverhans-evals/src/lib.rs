//! Action-mapping eval harness (spec build order step 6): does
//! *utterance + context* resolve to the *expected action* with the expected
//! utterance params?
//!
//! Cases run through the full agent pipeline — model selection, param fill,
//! validation — not just the raw provider, so a case passes only if a real
//! session would have rendered the expected proposal. Registry
//! `description`s are the tuning surface (spec §4); run the same suite
//! against every provider/model you deploy, especially weaker local models
//! (spec §8).

use std::fmt;

use serde::Deserialize;

use cleverhans_core::JsonMap;
use cleverhans_core::agent::{Agent, Session};
use cleverhans_core::envelope::{ClientEvent, Context, ServerEvent};

/// One eval case: an utterance in a context, and what should come of it.
#[derive(Debug, Clone, Deserialize)]
pub struct EvalCase {
    /// Human-readable case name.
    pub name: String,
    /// What the user says.
    pub utterance: String,
    /// The context snapshot the utterance is spoken in.
    #[serde(default)]
    pub context: Context,
    /// What the agent should do with it.
    pub expected: Expected,
}

/// The expected outcome of a case.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Expected {
    /// A proposal for this action, with at least these params (subset
    /// match — context-sourced params the case doesn't list are ignored).
    Action {
        /// The expected action ID.
        action_id: String,
        /// Params that must be present with exactly these values.
        #[serde(default)]
        params: JsonMap,
    },
    /// No proposal: the agent should answer conversationally (out-of-scope
    /// requests, ambiguity, chit-chat).
    Decline,
}

/// What the agent actually did.
#[derive(Debug, Clone, PartialEq)]
pub enum Actual {
    /// Proposed this action with these (fully filled) params.
    Proposed {
        /// Proposed action ID.
        action_id: String,
        /// Filled params from the proposal.
        params: JsonMap,
    },
    /// Answered conversationally without proposing.
    Declined,
    /// The turn produced a stream error (provider failure etc.).
    Error(String),
}

/// One case's result.
#[derive(Debug, Clone)]
pub struct EvalOutcome {
    /// The case that ran.
    pub case: EvalCase,
    /// What happened.
    pub actual: Actual,
    /// Whether `actual` satisfies `case.expected`.
    pub passed: bool,
}

/// A whole suite's results.
#[derive(Debug)]
pub struct EvalReport {
    /// Per-case outcomes, in input order.
    pub outcomes: Vec<EvalOutcome>,
}

impl EvalReport {
    /// Passed / total.
    #[must_use]
    pub fn passed(&self) -> usize {
        self.outcomes.iter().filter(|o| o.passed).count()
    }

    /// Fraction of cases passed, 0.0–1.0 (1.0 for an empty suite).
    #[must_use]
    pub fn accuracy(&self) -> f64 {
        if self.outcomes.is_empty() {
            return 1.0;
        }
        #[expect(
            clippy::cast_precision_loss,
            reason = "eval suites are far below 2^52 cases"
        )]
        {
            self.passed() as f64 / self.outcomes.len() as f64
        }
    }

    /// Whether every case passed.
    #[must_use]
    pub fn all_passed(&self) -> bool {
        self.passed() == self.outcomes.len()
    }
}

impl fmt::Display for EvalReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for outcome in &self.outcomes {
            let mark = if outcome.passed { "PASS" } else { "FAIL" };
            writeln!(f, "[{mark}] {}", outcome.case.name)?;
            if !outcome.passed {
                writeln!(f, "       expected: {:?}", outcome.case.expected)?;
                writeln!(f, "       actual:   {:?}", outcome.actual)?;
            }
        }
        writeln!(
            f,
            "{}/{} passed ({:.0}%)",
            self.passed(),
            self.outcomes.len(),
            self.accuracy() * 100.0
        )
    }
}

/// Parses a JSON array of [`EvalCase`]s.
///
/// # Errors
///
/// The underlying serde error for malformed case files.
pub fn load_cases(json: &str) -> Result<Vec<EvalCase>, serde_json::Error> {
    serde_json::from_str(json)
}

fn matches(expected: &Expected, actual: &Actual) -> bool {
    match (expected, actual) {
        (Expected::Decline, Actual::Declined) => true,
        (
            Expected::Action { action_id, params },
            Actual::Proposed {
                action_id: proposed,
                params: filled,
            },
        ) => {
            action_id == proposed
                && params
                    .iter()
                    .all(|(key, value)| filled.get(key) == Some(value))
        }
        _ => false,
    }
}

/// Runs one case in a fresh session.
pub async fn run_case<P: Clone + Send + Sync>(
    agent: &Agent<P>,
    principal: &P,
    case: EvalCase,
) -> EvalOutcome {
    let mut session = Session::new(principal.clone());
    agent
        .handle(
            &mut session,
            ClientEvent::Init {
                spec_version: cleverhans_core::SPEC_VERSION.to_owned(),
                context: case.context.clone(),
            },
        )
        .await;
    let events = agent
        .handle(
            &mut session,
            ClientEvent::UserMessage {
                text: case.utterance.clone(),
                client_msg_id: "eval".to_owned(),
            },
        )
        .await;

    let mut actual = Actual::Declined;
    for event in events {
        match event {
            ServerEvent::ActionProposal(proposal) => {
                actual = Actual::Proposed {
                    action_id: proposal.action_id,
                    params: proposal.params,
                };
                break;
            }
            ServerEvent::Error { message, .. } => {
                actual = Actual::Error(message);
                break;
            }
            _ => {}
        }
    }
    let passed = matches(&case.expected, &actual);
    EvalOutcome {
        case,
        actual,
        passed,
    }
}

/// Runs every case, each in its own fresh session.
pub async fn run_suite<P: Clone + Send + Sync>(
    agent: &Agent<P>,
    principal: &P,
    cases: Vec<EvalCase>,
) -> EvalReport {
    let mut outcomes = Vec::with_capacity(cases.len());
    for case in cases {
        outcomes.push(run_case(agent, principal, case).await);
    }
    EvalReport { outcomes }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use serde_json::json;

    use cleverhans_core::error::{HandlerError, LlmError};
    use cleverhans_core::registry::{
        ActionDef, BlockDef, ParamSource, ParamSpec, Registry, ValueType,
    };
    use cleverhans_core::seams::{
        ActionHandler, AuthzDecision, AuthzResolver, CompletionItem, CompletionRequest,
        ContextParamResolver, LlmProvider,
    };

    use super::*;

    #[derive(Clone)]
    struct User;

    /// Fake "model" that proposes `note.create` when the utterance mentions
    /// a note, and chats otherwise — enough to exercise the harness itself.
    struct KeywordLlm;

    #[async_trait]
    impl LlmProvider for KeywordLlm {
        async fn complete(
            &self,
            request: CompletionRequest,
        ) -> Result<Vec<CompletionItem>, LlmError> {
            let utterance = &request
                .messages
                .iter()
                .rev()
                .find(|turn| turn.role == cleverhans_core::seams::ChatRole::User)
                .expect("a user turn")
                .content;
            if utterance.contains("note") {
                let mut arguments = JsonMap::new();
                arguments.insert("text".to_owned(), json!("buy milk"));
                Ok(vec![CompletionItem::ToolCall {
                    name: "note.create".to_owned(),
                    arguments,
                }])
            } else {
                Ok(vec![CompletionItem::Text(
                    "Can't help with that.".to_owned(),
                )])
            }
        }
    }

    struct OkHandler;

    #[async_trait]
    impl ActionHandler<User> for OkHandler {
        async fn execute(
            &self,
            _params: &JsonMap,
            _principal: &User,
        ) -> Result<serde_json::Value, HandlerError> {
            Ok(serde_json::Value::Null)
        }
    }

    struct AllowAll;

    #[async_trait]
    impl AuthzResolver<User> for AllowAll {
        async fn authorize(
            &self,
            _principal: &User,
            _action_id: &str,
            _params: &JsonMap,
        ) -> AuthzDecision {
            AuthzDecision::Allow
        }
    }

    struct NoContextParams;

    impl ContextParamResolver for NoContextParams {
        fn resolve(
            &self,
            _action_id: &str,
            _param: &ParamSpec,
            _context: &Context,
        ) -> Option<serde_json::Value> {
            None
        }
    }

    fn agent() -> Agent<User> {
        let registry = Registry::builder()
            .block(BlockDef {
                block_type: "confirm".to_owned(),
                slots: vec![],
            })
            .action(
                ActionDef {
                    id: "note.create".to_owned(),
                    description: "Create a note".to_owned(),
                    params: vec![ParamSpec {
                        name: "text".to_owned(),
                        description: "Note body".to_owned(),
                        ty: ValueType::String,
                        source: ParamSource::Utterance,
                        required: true,
                    }],
                    block_type: "confirm".to_owned(),
                    mutates: false,
                    authz_key: "note".to_owned(),
                },
                Arc::new(OkHandler),
                None,
                None,
            )
            .build()
            .expect("valid registry");
        Agent::new(
            Arc::new(registry),
            Arc::new(KeywordLlm),
            Arc::new(AllowAll),
            Arc::new(NoContextParams),
        )
    }

    fn cases_json() -> &'static str {
        r#"[
            {
                "name": "note request maps to note.create with the body",
                "utterance": "note: buy milk",
                "expected": {"kind": "action", "action_id": "note.create",
                             "params": {"text": "buy milk"}}
            },
            {
                "name": "chit-chat declines",
                "utterance": "how are you?",
                "expected": {"kind": "decline"}
            },
            {
                "name": "wrong expectation fails",
                "utterance": "note: buy milk",
                "expected": {"kind": "decline"}
            }
        ]"#
    }

    #[tokio::test]
    async fn suite_scores_matches_and_mismatches() {
        let agent = agent();
        let cases = load_cases(cases_json()).expect("valid cases");

        let report = run_suite(&agent, &User, cases).await;

        assert_eq!(
            report.outcomes.iter().map(|o| o.passed).collect::<Vec<_>>(),
            vec![true, true, false],
            "report:\n{report}"
        );
        assert_eq!(report.passed(), 2);
    }

    #[tokio::test]
    async fn param_subset_match_ignores_extra_filled_params() {
        let agent = agent();
        let case = EvalCase {
            name: "params ignored when not listed".to_owned(),
            utterance: "note: buy milk".to_owned(),
            context: Context::default(),
            expected: Expected::Action {
                action_id: "note.create".to_owned(),
                params: JsonMap::new(),
            },
        };

        let outcome = run_case(&agent, &User, case).await;

        assert!(outcome.passed, "got {:?}", outcome.actual);
    }
}
