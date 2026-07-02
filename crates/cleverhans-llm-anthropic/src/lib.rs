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
use serde::Deserialize;
use serde_json::{Value, json};

use cleverhans_core::JsonMap;
use cleverhans_core::error::LlmError;
use cleverhans_core::seams::{
    ChatRole, CompletionChunk, CompletionItem, CompletionRequest, CompletionStream, LlmProvider,
};

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

/// The subset of a Messages API response the agent loop consumes. Unknown
/// block types (thinking, server tools, future additions) deserialize to
/// [`ResponseBlock::Other`] and are skipped — the API evolves additively.
#[derive(Debug, Deserialize)]
struct MessagesResponse {
    #[serde(default)]
    stop_reason: Option<String>,
    content: Vec<ResponseBlock>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ResponseBlock {
    Text {
        text: String,
    },
    ToolUse {
        name: String,
        #[serde(default)]
        input: JsonMap,
    },
    #[serde(other)]
    Other,
}

/// Maps a Messages API response body onto completion items.
///
/// # Errors
///
/// [`LlmError::Provider`] on a `refusal` stop reason or an unrecognizable
/// body.
fn parse_body(body: Value) -> Result<Vec<CompletionItem>, LlmError> {
    let response: MessagesResponse = serde_json::from_value(body)
        .map_err(|err| LlmError::Provider(format!("unrecognizable response body: {err}")))?;
    if response.stop_reason.as_deref() == Some("refusal") {
        return Err(LlmError::Provider(
            "the model provider declined this request (stop_reason: refusal)".to_owned(),
        ));
    }
    Ok(response
        .content
        .into_iter()
        .filter_map(|block| match block {
            ResponseBlock::Text { text } => Some(CompletionItem::Text(text)),
            ResponseBlock::ToolUse { name, input } => Some(CompletionItem::ToolCall {
                name: decode_tool_name(&name),
                arguments: input,
            }),
            ResponseBlock::Other => None,
        })
        .collect())
}

/// The subset of Messages API SSE events the agent loop consumes, typed.
/// Unknown event, block, and delta types deserialize to their `Other`
/// variants and are ignored — the stream protocol evolves additively, so an
/// unrecognized event must never break a session.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum SseEvent {
    ContentBlockStart {
        content_block: SseContentBlock,
    },
    ContentBlockDelta {
        delta: SseDelta,
    },
    ContentBlockStop,
    MessageDelta {
        delta: SseMessageDelta,
    },
    Error {
        error: Value,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum SseContentBlock {
    Text,
    ToolUse {
        name: String,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum SseDelta {
    TextDelta {
        text: String,
    },
    InputJsonDelta {
        partial_json: String,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
struct SseMessageDelta {
    #[serde(default)]
    stop_reason: Option<String>,
}

/// Accumulates SSE events into completion chunks. Text blocks stream as
/// deltas; `tool_use` blocks buffer their `input_json_delta` fragments and
/// yield one complete tool call at `content_block_stop`.
#[derive(Default)]
struct SseAccumulator {
    block: SseBlock,
}

#[derive(Default)]
enum SseBlock {
    #[default]
    None,
    Text,
    Tool {
        name: String,
        arguments_json: String,
    },
}

impl SseAccumulator {
    fn on_event(&mut self, event: SseEvent) -> Result<Vec<CompletionChunk>, LlmError> {
        match event {
            SseEvent::ContentBlockStart { content_block } => {
                self.block = match content_block {
                    SseContentBlock::Text => SseBlock::Text,
                    SseContentBlock::ToolUse { name } => SseBlock::Tool {
                        name,
                        arguments_json: String::new(),
                    },
                    SseContentBlock::Other => SseBlock::None,
                };
                Ok(Vec::new())
            }
            SseEvent::ContentBlockDelta { delta } => match delta {
                SseDelta::TextDelta { text } => Ok(vec![CompletionChunk::TextDelta(text)]),
                SseDelta::InputJsonDelta { partial_json } => {
                    if let SseBlock::Tool { arguments_json, .. } = &mut self.block {
                        arguments_json.push_str(&partial_json);
                    }
                    Ok(Vec::new())
                }
                SseDelta::Other => Ok(Vec::new()),
            },
            SseEvent::ContentBlockStop => match std::mem::take(&mut self.block) {
                SseBlock::Text => Ok(vec![CompletionChunk::TextDone]),
                SseBlock::Tool {
                    name,
                    arguments_json,
                } => {
                    let arguments: JsonMap = if arguments_json.trim().is_empty() {
                        JsonMap::new()
                    } else {
                        serde_json::from_str(&arguments_json).map_err(|err| {
                            LlmError::Provider(format!("unparseable tool arguments: {err}"))
                        })?
                    };
                    Ok(vec![CompletionChunk::ToolCall {
                        name: decode_tool_name(&name),
                        arguments,
                    }])
                }
                SseBlock::None => Ok(Vec::new()),
            },
            SseEvent::MessageDelta { delta } => {
                if delta.stop_reason.as_deref() == Some("refusal") {
                    return Err(LlmError::Provider(
                        "the model provider declined this request (stop_reason: refusal)"
                            .to_owned(),
                    ));
                }
                Ok(Vec::new())
            }
            SseEvent::Error { error } => Err(LlmError::Provider(format!(
                "anthropic stream error: {error}"
            ))),
            SseEvent::Other => Ok(Vec::new()),
        }
    }
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
        parse_body(payload)
    }

    async fn complete_stream(
        &self,
        request: CompletionRequest,
    ) -> Result<CompletionStream, LlmError> {
        use futures_util::StreamExt;

        let mut body = build_body(&self.config, &request);
        body["stream"] = json!(true);
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
        if !status.is_success() {
            let payload = response.text().await.unwrap_or_default();
            return Err(LlmError::Provider(format!(
                "anthropic api returned {status}: {payload}"
            )));
        }

        let mut bytes = response.bytes_stream();
        let stream = async_stream::stream! {
            let mut accumulator = SseAccumulator::default();
            let mut buffer = String::new();
            while let Some(frame) = bytes.next().await {
                let frame = match frame {
                    Ok(frame) => frame,
                    Err(err) => {
                        yield Err(LlmError::Provider(err.to_string()));
                        return;
                    }
                };
                buffer.push_str(&String::from_utf8_lossy(&frame));
                // SSE frames are line-delimited; a network chunk may split a
                // line, so only complete lines leave the buffer.
                while let Some(newline) = buffer.find('\n') {
                    let line: String = buffer.drain(..=newline).collect();
                    let Some(data) = line.trim().strip_prefix("data:") else {
                        continue;
                    };
                    let Ok(event) = serde_json::from_str::<SseEvent>(data.trim()) else {
                        continue;
                    };
                    match accumulator.on_event(event) {
                        Ok(chunks) => {
                            for chunk in chunks {
                                yield Ok(chunk);
                            }
                        }
                        Err(err) => {
                            yield Err(err);
                            return;
                        }
                    }
                }
            }
        };
        Ok(Box::pin(stream))
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

            let items = parse_body(body).expect("parseable");

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

            let result = parse_body(body);

            assert!(matches!(result, Err(LlmError::Provider(msg)) if msg.contains("refusal")));
        }

        #[test]
        fn sse_accumulator_streams_text_and_buffers_tool_calls() {
            let mut acc = SseAccumulator::default();
            let events = [
                json!({"type": "content_block_start", "index": 0,
                       "content_block": {"type": "text", "text": ""}}),
                json!({"type": "content_block_delta", "index": 0,
                       "delta": {"type": "text_delta", "text": "Prop"}}),
                json!({"type": "content_block_delta", "index": 0,
                       "delta": {"type": "text_delta", "text": "osing."}}),
                json!({"type": "content_block_stop", "index": 0}),
                json!({"type": "content_block_start", "index": 1,
                       "content_block": {"type": "tool_use", "id": "toolu_1",
                                          "name": "transaction__coBuyer__remove", "input": {}}}),
                json!({"type": "content_block_delta", "index": 1,
                       "delta": {"type": "input_json_delta", "partial_json": "{\"count"}}),
                json!({"type": "content_block_delta", "index": 1,
                       "delta": {"type": "input_json_delta", "partial_json": "ry\": \"NO\"}"}}),
                json!({"type": "content_block_stop", "index": 1}),
            ];

            let chunks: Vec<_> = events
                .iter()
                .flat_map(|event| {
                    let event: SseEvent =
                        serde_json::from_value(event.clone()).expect("typed event");
                    acc.on_event(event).expect("valid event")
                })
                .collect();

            let mut arguments = JsonMap::new();
            arguments.insert("country".to_owned(), json!("NO"));
            assert_eq!(
                chunks,
                vec![
                    CompletionChunk::TextDelta("Prop".to_owned()),
                    CompletionChunk::TextDelta("osing.".to_owned()),
                    CompletionChunk::TextDone,
                    CompletionChunk::ToolCall {
                        name: "transaction.coBuyer.remove".to_owned(),
                        arguments,
                    },
                ]
            );
        }

        #[test]
        fn sse_refusal_stop_reason_is_a_provider_error() {
            let mut acc = SseAccumulator::default();

            let event: SseEvent = serde_json::from_value(
                json!({"type": "message_delta", "delta": {"stop_reason": "refusal"}, "usage": {}}),
            )
            .expect("typed event");

            let result = acc.on_event(event);

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

            let items = parse_body(body).expect("parseable");

            assert_eq!(items, vec![CompletionItem::Text("Done.".to_owned())]);
        }
    }
}
