//! Ollama implementation of the CleverHans [`LlmProvider`] seam — the
//! fully-local, zero-egress leg of the self-hosting story (spec §8, §9.4).
//!
//! Talks to Ollama's `POST /api/chat` with tool definitions. Local models
//! are weaker at action selection than frontier models, which is exactly
//! what the framework is built for: registry descriptions carry more
//! weight, validation catches bad selections, and propose-only makes a
//! wrong selection safe. Run the action-mapping evals against whatever
//! model you deploy.
//!
//! Tool names use the same `.` → `__` mangling as the Anthropic provider so
//! registries behave identically across providers.

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use cleverhans_core::JsonMap;
use cleverhans_core::error::LlmError;
use cleverhans_core::seams::{
    ChatRole, CompletionChunk, CompletionItem, CompletionRequest, CompletionStream, LlmProvider,
};

/// Configuration for [`OllamaProvider`].
#[derive(Debug, Clone)]
pub struct OllamaConfig {
    /// Model name as known to the Ollama daemon (e.g. `"qwen3"`,
    /// `"llama3.1"`). Pick a model with tool-use support.
    pub model: String,
    /// Daemon origin.
    pub base_url: String,
}

impl OllamaConfig {
    /// Config against a local daemon on the default port.
    #[must_use]
    pub fn new(model: String) -> Self {
        Self {
            model,
            base_url: "http://localhost:11434".to_owned(),
        }
    }
}

/// [`LlmProvider`] over Ollama's chat API. No credential at all — the whole
/// point of this provider is zero egress.
pub struct OllamaProvider {
    config: OllamaConfig,
    http: reqwest::Client,
}

impl OllamaProvider {
    /// Builds a provider with its own HTTP client.
    #[must_use]
    pub fn new(config: OllamaConfig) -> Self {
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

/// Renders a [`CompletionRequest`] as an `/api/chat` body. Ollama accepts a
/// system role in `messages`, so the system turn stays in place; `Tool`
/// turns become user-role messages for the same reason as in the Anthropic
/// provider — proposal outcomes resolve out-of-band, there is no
/// tool-call/tool-result pairing to preserve.
fn build_body(config: &OllamaConfig, request: &CompletionRequest, stream: bool) -> Value {
    let messages: Vec<Value> = request
        .messages
        .iter()
        .map(|turn| {
            let role = match turn.role {
                ChatRole::System => "system",
                ChatRole::User | ChatRole::Tool => "user",
                ChatRole::Assistant => "assistant",
            };
            json!({"role": role, "content": turn.content})
        })
        .collect();
    let tools: Vec<Value> = request
        .tools
        .iter()
        .map(|tool| {
            json!({
                "type": "function",
                "function": {
                    "name": encode_tool_name(&tool.name),
                    "description": tool.description,
                    "parameters": tool.parameters,
                },
            })
        })
        .collect();

    let mut body = json!({
        "model": config.model,
        "messages": messages,
        "stream": stream,
    });
    if !tools.is_empty() {
        body["tools"] = json!(tools);
    }
    body
}

/// One `/api/chat` response object — the whole body when `stream: false`,
/// one NDJSON line when `stream: true`. Unknown fields are ignored; tool
/// calls arrive complete, never fragmented.
#[derive(Debug, Deserialize)]
struct ChatResponse {
    #[serde(default)]
    message: Option<ChatMessage>,
    #[serde(default)]
    done: bool,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ChatMessage {
    #[serde(default)]
    content: String,
    #[serde(default)]
    tool_calls: Vec<ToolCall>,
}

#[derive(Debug, Deserialize)]
struct ToolCall {
    function: ToolCallFunction,
}

#[derive(Debug, Deserialize)]
struct ToolCallFunction {
    name: String,
    #[serde(default)]
    arguments: JsonMap,
}

fn items_from(message: ChatMessage) -> Vec<CompletionItem> {
    let mut items = Vec::new();
    if !message.content.is_empty() {
        items.push(CompletionItem::Text(message.content));
    }
    for call in message.tool_calls {
        items.push(CompletionItem::ToolCall {
            name: decode_tool_name(&call.function.name),
            arguments: call.function.arguments,
        });
    }
    items
}

/// Folds streamed NDJSON lines into completion chunks: `content` fragments
/// stream as text deltas (closed at `done`), tool calls pass through whole.
#[derive(Default)]
struct NdjsonAccumulator {
    text_open: bool,
}

impl NdjsonAccumulator {
    fn on_line(&mut self, line: &str) -> Result<Vec<CompletionChunk>, LlmError> {
        let response: ChatResponse = serde_json::from_str(line)
            .map_err(|err| LlmError::Provider(format!("unrecognizable chat line: {err}")))?;
        if let Some(error) = response.error {
            return Err(LlmError::Provider(format!("ollama error: {error}")));
        }
        let mut chunks = Vec::new();
        if let Some(message) = response.message {
            if !message.content.is_empty() {
                self.text_open = true;
                chunks.push(CompletionChunk::TextDelta(message.content));
            }
            for call in message.tool_calls {
                if std::mem::take(&mut self.text_open) {
                    chunks.push(CompletionChunk::TextDone);
                }
                chunks.push(CompletionChunk::ToolCall {
                    name: decode_tool_name(&call.function.name),
                    arguments: call.function.arguments,
                });
            }
        }
        if response.done && std::mem::take(&mut self.text_open) {
            chunks.push(CompletionChunk::TextDone);
        }
        Ok(chunks)
    }
}

#[async_trait]
impl LlmProvider for OllamaProvider {
    async fn complete(&self, request: CompletionRequest) -> Result<Vec<CompletionItem>, LlmError> {
        let body = build_body(&self.config, &request, false);
        let response = self
            .http
            .post(format!("{}/api/chat", self.config.base_url))
            .json(&body)
            .send()
            .await
            .map_err(|err| LlmError::Provider(err.to_string()))?;
        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|err| LlmError::Provider(err.to_string()))?;
        if !status.is_success() {
            return Err(LlmError::Provider(format!(
                "ollama returned {status}: {text}"
            )));
        }
        let parsed: ChatResponse = serde_json::from_str(&text)
            .map_err(|err| LlmError::Provider(format!("unrecognizable response body: {err}")))?;
        if let Some(error) = parsed.error {
            return Err(LlmError::Provider(format!("ollama error: {error}")));
        }
        Ok(parsed.message.map(items_from).unwrap_or_default())
    }

    async fn complete_stream(
        &self,
        request: CompletionRequest,
    ) -> Result<CompletionStream, LlmError> {
        use futures_util::StreamExt;

        let body = build_body(&self.config, &request, true);
        let response = self
            .http
            .post(format!("{}/api/chat", self.config.base_url))
            .json(&body)
            .send()
            .await
            .map_err(|err| LlmError::Provider(err.to_string()))?;
        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(LlmError::Provider(format!(
                "ollama returned {status}: {text}"
            )));
        }

        let mut bytes = response.bytes_stream();
        let stream = async_stream::stream! {
            let mut accumulator = NdjsonAccumulator::default();
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
                while let Some(newline) = buffer.find('\n') {
                    let line: String = buffer.drain(..=newline).collect();
                    if line.trim().is_empty() {
                        continue;
                    }
                    match accumulator.on_line(line.trim()) {
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
        fn system_turn_stays_in_messages() {
            let body = build_body(&OllamaConfig::new("qwen3".to_owned()), &request(), false);

            assert_eq!(body["messages"][0]["role"], json!("system"));
            assert_eq!(body["stream"], json!(false));
        }

        #[test]
        fn tools_use_function_shape_with_mangled_names() {
            let body = build_body(&OllamaConfig::new("qwen3".to_owned()), &request(), false);

            assert_eq!(
                body["tools"][0]["function"]["name"],
                json!("transaction__coBuyer__remove")
            );
        }
    }

    mod parse {
        use super::*;

        #[test]
        fn tool_calls_decode_back_to_action_ids() {
            let parsed: ChatResponse = serde_json::from_value(json!({
                "message": {
                    "role": "assistant",
                    "content": "",
                    "tool_calls": [{"function": {
                        "name": "transaction__coBuyer__remove",
                        "arguments": {"country": "NO"},
                    }}],
                },
                "done": true,
            }))
            .expect("valid response");

            let items = items_from(parsed.message.expect("message"));

            let mut arguments = JsonMap::new();
            arguments.insert("country".to_owned(), json!("NO"));
            assert_eq!(
                items,
                vec![CompletionItem::ToolCall {
                    name: "transaction.coBuyer.remove".to_owned(),
                    arguments,
                }]
            );
        }
    }

    mod ndjson_accumulator {
        use super::*;

        #[test]
        fn streams_text_deltas_and_closes_on_done() {
            let mut acc = NdjsonAccumulator::default();
            let lines = [
                r#"{"message": {"role": "assistant", "content": "Hel"}, "done": false}"#,
                r#"{"message": {"role": "assistant", "content": "lo."}, "done": false}"#,
                r#"{"message": {"role": "assistant", "content": ""}, "done": true}"#,
            ];

            let chunks: Vec<_> = lines
                .iter()
                .flat_map(|line| acc.on_line(line).expect("valid line"))
                .collect();

            assert_eq!(
                chunks,
                vec![
                    CompletionChunk::TextDelta("Hel".to_owned()),
                    CompletionChunk::TextDelta("lo.".to_owned()),
                    CompletionChunk::TextDone,
                ]
            );
        }

        #[test]
        fn tool_call_closes_any_open_text_segment_first() {
            let mut acc = NdjsonAccumulator::default();
            let lines = [
                r#"{"message": {"role": "assistant", "content": "Proposing."}, "done": false}"#,
                r#"{"message": {"role": "assistant", "content": "", "tool_calls": [{"function": {"name": "note__create", "arguments": {}}}]}, "done": true}"#,
            ];

            let chunks: Vec<_> = lines
                .iter()
                .flat_map(|line| acc.on_line(line).expect("valid line"))
                .collect();

            assert_eq!(
                chunks,
                vec![
                    CompletionChunk::TextDelta("Proposing.".to_owned()),
                    CompletionChunk::TextDone,
                    CompletionChunk::ToolCall {
                        name: "note.create".to_owned(),
                        arguments: JsonMap::new(),
                    },
                ]
            );
        }

        #[test]
        fn daemon_error_lines_become_provider_errors() {
            let mut acc = NdjsonAccumulator::default();

            let result = acc.on_line(r#"{"error": "model not found"}"#);

            assert!(
                matches!(result, Err(LlmError::Provider(msg)) if msg.contains("model not found"))
            );
        }
    }
}
