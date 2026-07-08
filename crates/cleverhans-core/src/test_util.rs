//! Deterministic doubles for testing an integration without a live model
//! (feature `test-util`).
//!
//! Enable it for your test target only:
//!
//! ```toml
//! [dev-dependencies]
//! cleverhans-core = { version = "0.1", features = ["test-util"] }
//! ```
//!
//! [`ScriptedLlm`] drives the whole propose → confirm → execute pipeline —
//! registry, validation, authz, handlers — with the model replaced by a
//! script, so integration tests are fast, offline, and exact.

use std::collections::VecDeque;
use std::sync::Mutex;

use async_trait::async_trait;

use crate::error::LlmError;
use crate::seams::{CompletionItem, CompletionRequest, LlmProvider};

/// Scripted [`LlmProvider`]: one item list per completion call, in order,
/// recording every request it receives. Exhaustion is a loud scripting
/// error, not a silent decline.
///
/// ```
/// use std::sync::Arc;
/// use cleverhans_core::seams::CompletionItem;
/// use cleverhans_core::slots;
/// use cleverhans_core::test_util::ScriptedLlm;
///
/// let llm = Arc::new(ScriptedLlm::new([
///     vec![CompletionItem::ToolCall {
///         name: "document.rename".to_owned(),
///         arguments: slots! { "title": "Roadmap" },
///     }],
///     vec![CompletionItem::Text("Renamed.".to_owned())],
/// ]));
/// // pass `llm.clone()` to `Agent::new`, keep `llm` for assertions:
/// assert_eq!(llm.requests().len(), 0);
/// ```
pub struct ScriptedLlm {
    turns: Mutex<VecDeque<Vec<CompletionItem>>>,
    requests: Mutex<Vec<CompletionRequest>>,
}

impl ScriptedLlm {
    /// Builds a provider from scripted turns, consumed front to back.
    #[must_use]
    pub fn new(turns: impl IntoIterator<Item = Vec<CompletionItem>>) -> Self {
        Self {
            turns: Mutex::new(turns.into_iter().collect()),
            requests: Mutex::new(Vec::new()),
        }
    }

    /// Every [`CompletionRequest`] received so far — assert on system turns,
    /// history shape, or the tool list the registry produced.
    #[must_use]
    pub fn requests(&self) -> Vec<CompletionRequest> {
        self.requests.lock().expect("requests lock").clone()
    }
}

#[async_trait]
impl LlmProvider for ScriptedLlm {
    async fn complete(&self, request: CompletionRequest) -> Result<Vec<CompletionItem>, LlmError> {
        self.requests.lock().expect("requests lock").push(request);
        self.turns
            .lock()
            .expect("turns lock")
            .pop_front()
            .ok_or_else(|| {
                LlmError::Provider(
                    "scripted llm exhausted — the script has fewer turns than the \
                     agent requested (scripting error)"
                        .to_owned(),
                )
            })
    }
}
