//! Anthropic Claude API implementation of the CleverHans [`LlmProvider`]
//! seam (spec §9.4).
//!
//! Raw HTTP against `POST /v1/messages` — Anthropic ships no official Rust
//! SDK. The provider key is the agent's only credential (spec §10); it never
//! grants any access to the host application.
//!
//! # Tool-name mangling
//!
//! CleverHans action IDs are dotted keys (`transaction.coBuyer.remove`), but
//! Anthropic tool names must match `[a-zA-Z0-9_-]{1,64}`. Dots are encoded
//! as `__` on the way out and decoded on the way back. An action ID that
//! itself contains `__` would round-trip wrong — the decoded ID then simply
//! fails validation as unknown (propose-only makes a bad mapping safe, not
//! silent), but avoid `__` in action IDs when using this provider.

use async_trait::async_trait;
use serde_json::{Value, json};

use cleverhans_core::JsonMap;
use cleverhans_core::error::LlmError;
use cleverhans_core::seams::{ChatRole, CompletionItem, CompletionRequest, LlmProvider};

/// Default model; override via [`AnthropicConfig::model`].
pub const DEFAULT_MODEL: &str = "claude-opus-4-8";

/// Configuration for [`AnthropicProvider`].
#[derive(Debug, Clone)]
pub struct AnthropicConfig {
    /// Anthropic API key — the agent's only credential.
    pub api_key: String,
    /// Model ID, e.g. [`DEFAULT_MODEL`].
    pub model: String,
    /// Per-response output-token cap.
    pub max_tokens: u32,
    /// API origin; override for gateways or tests.
    pub base_url: String,
}

impl AnthropicConfig {
    /// Config with recommended defaults for the given key.
    #[must_use]
    pub fn new(api_key: String) -> Self {
        Self {
            api_key,
            model: DEFAULT_MODEL.to_owned(),
            max_tokens: 4096,
            base_url: "https://api.anthropic.com".to_owned(),
        }
    }
}

/// [`LlmProvider`] over the Anthropic Messages API.
pub struct AnthropicProvider {
    config: AnthropicConfig,
    http: reqwest::Client,
}

impl AnthropicProvider {
    /// Builds a provider with its own HTTP client.
    #[must_use]
    pub fn new(config: AnthropicConfig) -> Self {
        Self {
            config,
            http: reqwest::Client::new(),
        }
    }
}

fn encode_tool_name(action_id: &str) -> String {
    action_id.replace('.', "__")
}

fn decode_tool_name(tool_name: &str) -> String {
    tool_name.replace("__", ".")
}

/// Renders a [`CompletionRequest`] as a Messages API body. `System` turns
/// become the top-level `system` parameter (the API rejects a system role in
/// `messages`); `Tool` turns become user-role messages — the framework's
/// tool notes are conversational context, not Anthropic `tool_result`
/// blocks, because proposals resolve out-of-band (user confirmation), not
/// within one assistant turn.
fn build_body(config: &AnthropicConfig, request: &CompletionRequest) -> Value {
    let mut system_parts = Vec::new();
    let mut messages = Vec::new();
    for turn in &request.messages {
        match turn.role {
            ChatRole::System => system_parts.push(turn.content.as_str()),
            ChatRole::User => messages.push(json!({"role": "user", "content": turn.content})),
            ChatRole::Assistant => {
                messages.push(json!({"role": "assistant", "content": turn.content}));
            }
            ChatRole::Tool => {
                messages.push(json!({"role": "user", "content": turn.content}));
            }
        }
    }
    let tools: Vec<Value> = request
        .tools
        .iter()
        .map(|tool| {
            json!({
                "name": encode_tool_name(&tool.name),
                "description": tool.description,
                "input_schema": tool.parameters,
            })
        })
        .collect();

    let mut body = json!({
        "model": config.model,
        "max_tokens": config.max_tokens,
        "messages": messages,
    });
    if !system_parts.is_empty() {
        body["system"] = json!(system_parts.join("\n\n"));
    }
    if !tools.is_empty() {
        body["tools"] = json!(tools);
    }
    body
}

/// Maps a Messages API response body onto completion items.
///
/// # Errors
///
/// [`LlmError::Provider`] on a `refusal` stop reason or an unrecognizable
/// body.
fn parse_body(body: &Value) -> Result<Vec<CompletionItem>, LlmError> {
    if body["stop_reason"] == json!("refusal") {
        return Err(LlmError::Provider(
            "the model provider declined this request (stop_reason: refusal)".to_owned(),
        ));
    }
    let blocks = body["content"]
        .as_array()
        .ok_or_else(|| LlmError::Provider(format!("response has no content array: {body}")))?;
    let mut items = Vec::new();
    for block in blocks {
        match block["type"].as_str() {
            Some("text") => {
                if let Some(text) = block["text"].as_str() {
                    items.push(CompletionItem::Text(text.to_owned()));
                }
            }
            Some("tool_use") => {
                let name = block["name"]
                    .as_str()
                    .ok_or_else(|| LlmError::Provider("tool_use without name".to_owned()))?;
                let arguments: JsonMap = block["input"].as_object().cloned().unwrap_or_default();
                items.push(CompletionItem::ToolCall {
                    name: decode_tool_name(name),
                    arguments,
                });
            }
            // Thinking and other block types carry nothing the agent loop
            // consumes; skip them.
            _ => {}
        }
    }
    Ok(items)
}

#[async_trait]
impl LlmProvider for AnthropicProvider {
    async fn complete(&self, request: CompletionRequest) -> Result<Vec<CompletionItem>, LlmError> {
        let body = build_body(&self.config, &request);
        let response = self
            .http
            .post(format!("{}/v1/messages", self.config.base_url))
            .header("x-api-key", &self.config.api_key)
            .header("anthropic-version", "2023-06-01")
            .json(&body)
            .send()
            .await
            .map_err(|err| LlmError::Provider(err.to_string()))?;
        let status = response.status();
        let payload: Value = response
            .json()
            .await
            .map_err(|err| LlmError::Provider(err.to_string()))?;
        if !status.is_success() {
            return Err(LlmError::Provider(format!(
                "anthropic api returned {status}: {payload}"
            )));
        }
        parse_body(&payload)
    }
}

#[cfg(test)]
mod tests {
    use cleverhans_core::seams::{ChatTurn, ToolDef};

    use super::*;

    fn request() -> CompletionRequest {
        CompletionRequest {
            messages: vec![
                ChatTurn {
                    role: ChatRole::System,
                    content: "You propose, never execute.".to_owned(),
                },
                ChatTurn {
                    role: ChatRole::User,
                    content: "remove the co-buyer".to_owned(),
                },
                ChatTurn {
                    role: ChatRole::Tool,
                    content: "proposal rejected by validation: unknown action".to_owned(),
                },
            ],
            tools: vec![ToolDef {
                name: "transaction.coBuyer.remove".to_owned(),
                description: "Remove the co-buyer".to_owned(),
                parameters: json!({"type": "object", "properties": {}, "required": []}),
            }],
        }
    }

    mod build_body {
        use super::*;

        #[test]
        fn system_turn_becomes_top_level_system_param() {
            let body = build_body(&AnthropicConfig::new("k".to_owned()), &request());

            assert_eq!(body["system"], json!("You propose, never execute."));
            let roles: Vec<_> = body["messages"]
                .as_array()
                .expect("messages")
                .iter()
                .map(|m| m["role"].as_str().unwrap_or_default())
                .collect();
            assert_eq!(roles, vec!["user", "user"], "no system role in messages");
        }

        #[test]
        fn dotted_action_ids_are_mangled_into_valid_tool_names() {
            let body = build_body(&AnthropicConfig::new("k".to_owned()), &request());

            assert_eq!(
                body["tools"][0]["name"],
                json!("transaction__coBuyer__remove")
            );
        }
    }

    mod parse_body {
        use super::*;

        #[test]
        fn decodes_tool_use_back_to_action_id() {
            let body = json!({
                "stop_reason": "tool_use",
                "content": [
                    {"type": "text", "text": "Proposing."},
                    {"type": "tool_use", "id": "toolu_1",
                     "name": "transaction__coBuyer__remove", "input": {}},
                ],
            });

            let items = parse_body(&body).expect("parseable");

            assert_eq!(
                items,
                vec![
                    CompletionItem::Text("Proposing.".to_owned()),
                    CompletionItem::ToolCall {
                        name: "transaction.coBuyer.remove".to_owned(),
                        arguments: JsonMap::new(),
                    },
                ]
            );
        }

        #[test]
        fn refusal_stop_reason_is_a_provider_error() {
            let body = json!({"stop_reason": "refusal", "content": []});

            let result = parse_body(&body);

            assert!(matches!(result, Err(LlmError::Provider(msg)) if msg.contains("refusal")));
        }

        #[test]
        fn skips_thinking_blocks() {
            let body = json!({
                "stop_reason": "end_turn",
                "content": [
                    {"type": "thinking", "thinking": ""},
                    {"type": "text", "text": "Done."},
                ],
            });

            let items = parse_body(&body).expect("parseable");

            assert_eq!(items, vec![CompletionItem::Text("Done.".to_owned())]);
        }
    }
}
