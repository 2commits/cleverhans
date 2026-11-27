//! OpenAI-compatible Chat Completions implementation of the CleverHans
//! [`LlmProvider`] seam (spec §9.4) — the self-hosted leg for vLLM, SGLang,
//! NIM, Ollama's `/v1`, and hosted compatible APIs.
//!
//! Talks to `POST {base_url}/chat/completions` with tool definitions. The
//! base URL follows the OpenAI SDK convention and includes the version path
//! (`http://localhost:8000/v1`). It has no default: the framework never
//! sends prompts to a third party unless the host names the server. The API
//! key is optional (local servers often have none) and travels as a bearer
//! token; the client follows no redirects, so the key never reaches a host
//! other than the configured one.
//!
//! Reasoning models: a server with a reasoning parser returns the model's
//! reasoning in `reasoning` / `reasoning_content` fields. Those are ignored —
//! reasoning is never surfaced as chat text. A server without a reasoning
//! parser returns it inside `content`, which this provider cannot tell apart
//! from the answer; start the server with its reasoning parser.
//!
//! Tool names use the same `.` → `__` mangling as the Anthropic and Ollama
//! providers so registries behave identically across providers.

use std::collections::BTreeMap;
use std::fmt;

use async_trait::async_trait;
use reqwest::StatusCode;
use serde::Deserialize;
use serde_json::{Value, json};

use cleverhans_core::JsonMap;
use cleverhans_core::error::LlmError;
use cleverhans_core::seams::{
    ChatRole, CompletionChunk, CompletionItem, CompletionRequest, CompletionStream, LlmProvider,
};

/// Configuration for [`OpenAiProvider`].
#[derive(Clone)]
pub struct OpenAiConfig {
    /// Model name as the server knows it (vLLM: the served model name, e.g.
    /// `"Qwen/Qwen3-32B"`). Pick a model with tool-use support.
    pub model: String,
    /// API base including the version path, e.g. `http://localhost:8000/v1`.
    /// The provider posts to `{base_url}/chat/completions`; a trailing slash
    /// is tolerated.
    pub base_url: String,
    /// Sent as `Authorization: Bearer <key>` when set; no header otherwise.
    pub api_key: Option<String>,
}

impl OpenAiConfig {
    /// Config for the given server and model, without an API key.
    #[must_use]
    pub fn new(base_url: String, model: String) -> Self {
        Self {
            model,
            base_url,
            api_key: None,
        }
    }
}

/// Redacts the API key.
impl fmt::Debug for OpenAiConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OpenAiConfig")
            .field("model", &self.model)
            .field("base_url", &self.base_url)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

/// [`LlmProvider`] over an OpenAI-compatible Chat Completions API.
pub struct OpenAiProvider {
    config: OpenAiConfig,
    http: reqwest::Client,
}

impl OpenAiProvider {
    /// Builds a provider with its own HTTP client, which follows no
    /// redirects.
    ///
    /// # Panics
    ///
    /// When the TLS backend cannot be initialized — the same condition under
    /// which `reqwest::Client::new` panics.
    #[must_use]
    pub fn new(config: OpenAiConfig) -> Self {
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("TLS backend initializes");
        Self { config, http }
    }

    /// Builds (does not send) the `POST /chat/completions` request.
    fn request(&self, body: &Value) -> Result<reqwest::Request, LlmError> {
        let mut builder = self.http.post(endpoint(&self.config.base_url)).json(body);
        if let Some(api_key) = &self.config.api_key {
            builder = builder.bearer_auth(api_key);
        }
        builder
            .build()
            .map_err(|err| LlmError::Provider(err.to_string()))
    }

    async fn send(&self, body: &Value) -> Result<reqwest::Response, LlmError> {
        self.http
            .execute(self.request(body)?)
            .await
            .map_err(|err| LlmError::Provider(err.to_string()))
    }
}

fn endpoint(base_url: &str) -> String {
    format!("{}/chat/completions", base_url.trim_end_matches('/'))
}

fn encode_tool_name(action_id: &str) -> String {
    action_id.replace('.', "__")
}

fn decode_tool_name(tool_name: &str) -> String {
    tool_name.replace("__", ".")
}

/// Renders a [`CompletionRequest`] as a Chat Completions body. The system
/// turn stays in `messages`; `Tool` turns become user-role messages for the
/// same reason as in the Anthropic provider — proposal outcomes resolve
/// out-of-band, there is no tool-call/tool-result pairing to preserve, and
/// OpenAI-format servers reject a `tool` message without a matching
/// `tool_call_id`. No `tool_choice`, sampling parameters or `max_tokens`:
/// the server's defaults apply.
fn build_body(config: &OpenAiConfig, request: &CompletionRequest, stream: bool) -> Value {
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

/// The error for a non-success status. vLLM answers 400 to a request with
/// tools when it was started without tool-call parsing; the hint names the
/// two flags and never replaces the server's own body.
fn status_error(status: StatusCode, body: &str) -> LlmError {
    let mut message = format!("openai-compatible server returned {status}: {body}");
    if status == StatusCode::BAD_REQUEST
        && (body.contains("enable-auto-tool-choice") || body.contains("tool-call-parser"))
    {
        message.push_str(
            " (hint: vLLM serves tool calls only when started with \
             `--enable-auto-tool-choice` and `--tool-call-parser <parser>`)",
        );
    }
    LlmError::Provider(message)
}

/// An error reported inside a response body or stream chunk: the OpenAI
/// shape `{"error": {...}}`, or the older vLLM shape `{"object": "error",
/// "message": ...}`.
fn body_error(
    error: Option<Value>,
    object: Option<&str>,
    message: Option<String>,
) -> Option<LlmError> {
    if let Some(error) = error {
        return Some(LlmError::Provider(format!(
            "openai-compatible server error: {error}"
        )));
    }
    (object == Some("error")).then(|| {
        LlmError::Provider(format!(
            "openai-compatible server error: {}",
            message.unwrap_or_default()
        ))
    })
}

/// Decodes `function.arguments`: a JSON string holding a JSON object. An
/// empty string or a missing value is an empty object.
fn decode_arguments(action_id: &str, raw: Option<Value>) -> Result<JsonMap, LlmError> {
    let text = match raw {
        None | Some(Value::Null) => return Ok(JsonMap::new()),
        Some(Value::String(text)) => text,
        Some(other) => {
            return Err(LlmError::Provider(format!(
                "tool `{action_id}`: arguments are not a JSON string: {other}"
            )));
        }
    };
    if text.trim().is_empty() {
        return Ok(JsonMap::new());
    }
    match serde_json::from_str::<Value>(&text) {
        Ok(Value::Object(arguments)) => Ok(arguments),
        Ok(other) => Err(LlmError::Provider(format!(
            "tool `{action_id}`: arguments are not a JSON object: {other}"
        ))),
        Err(err) => Err(LlmError::Provider(format!(
            "tool `{action_id}`: unparseable arguments: {err}"
        ))),
    }
}

/// A non-streaming Chat Completions body. Unknown fields — `id`, `usage`,
/// `reasoning`, `reasoning_content`, … — are ignored.
#[derive(Debug, Deserialize)]
struct ChatCompletion {
    #[serde(default)]
    choices: Vec<Choice>,
    #[serde(default)]
    error: Option<Value>,
    #[serde(default)]
    object: Option<String>,
    #[serde(default)]
    message: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Choice {
    #[serde(default)]
    message: Option<ResponseMessage>,
}

#[derive(Debug, Deserialize)]
struct ResponseMessage {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<ResponseToolCall>>,
}

#[derive(Debug, Deserialize)]
struct ResponseToolCall {
    function: ResponseFunction,
}

#[derive(Debug, Deserialize)]
struct ResponseFunction {
    name: String,
    #[serde(default)]
    arguments: Option<Value>,
}

/// Maps a non-streaming body onto completion items. Whitespace-only content
/// (common before a tool call from a reasoning model) is not a message.
///
/// # Errors
///
/// [`LlmError::Provider`] on an error object, an unrecognizable body, or
/// tool arguments that are not a JSON object.
fn parse_body(text: &str) -> Result<Vec<CompletionItem>, LlmError> {
    let completion: ChatCompletion = serde_json::from_str(text)
        .map_err(|err| LlmError::Provider(format!("unrecognizable response body: {err}")))?;
    if let Some(err) = body_error(
        completion.error,
        completion.object.as_deref(),
        completion.message,
    ) {
        return Err(err);
    }
    let Some(message) = completion
        .choices
        .into_iter()
        .next()
        .and_then(|choice| choice.message)
    else {
        return Ok(Vec::new());
    };
    let mut items = Vec::new();
    if let Some(content) = message.content
        && !content.trim().is_empty()
    {
        items.push(CompletionItem::Text(content));
    }
    for call in message.tool_calls.unwrap_or_default() {
        let name = decode_tool_name(&call.function.name);
        let arguments = decode_arguments(&name, call.function.arguments)?;
        items.push(CompletionItem::ToolCall { name, arguments });
    }
    Ok(items)
}

/// One streamed `chat.completion.chunk`. Unknown fields — including
/// `delta.reasoning` / `delta.reasoning_content` — are ignored.
#[derive(Debug, Deserialize)]
struct StreamChunk {
    #[serde(default)]
    choices: Vec<StreamChoice>,
    #[serde(default)]
    error: Option<Value>,
    #[serde(default)]
    object: Option<String>,
    #[serde(default)]
    message: Option<String>,
}

#[derive(Debug, Deserialize)]
struct StreamChoice {
    #[serde(default)]
    delta: Option<StreamDelta>,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct StreamDelta {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<StreamToolCall>>,
}

#[derive(Debug, Deserialize)]
struct StreamToolCall {
    #[serde(default)]
    index: Option<usize>,
    #[serde(default)]
    function: Option<StreamFunction>,
}

#[derive(Debug, Deserialize)]
struct StreamFunction {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

#[derive(Debug, Default)]
struct PartialToolCall {
    name: String,
    arguments: String,
}

/// Folds streamed chunks into completion chunks. Content fragments stream as
/// text deltas; tool-call fragments accumulate per `index` and leave as
/// whole calls, in index order, once the choice finishes (or the stream
/// ends), after the open text segment is closed. Leading whitespace is held
/// back until real text arrives, so a reasoning model's blank lines before a
/// tool call never become an empty chat message.
#[derive(Default)]
struct SseAccumulator {
    text_open: bool,
    pending_whitespace: String,
    calls: BTreeMap<usize, PartialToolCall>,
}

impl SseAccumulator {
    fn on_data(&mut self, data: &str) -> Result<Vec<CompletionChunk>, LlmError> {
        let chunk: StreamChunk = serde_json::from_str(data)
            .map_err(|err| LlmError::Provider(format!("unrecognizable stream chunk: {err}")))?;
        if let Some(err) = body_error(chunk.error, chunk.object.as_deref(), chunk.message) {
            return Err(err);
        }
        let mut chunks = Vec::new();
        // The usage chunk carries an empty `choices` array.
        let Some(choice) = chunk.choices.into_iter().next() else {
            return Ok(chunks);
        };
        if let Some(delta) = choice.delta {
            if let Some(content) = delta.content {
                self.on_content(content, &mut chunks);
            }
            for (position, call) in delta.tool_calls.unwrap_or_default().into_iter().enumerate() {
                let entry = self
                    .calls
                    .entry(call.index.unwrap_or(position))
                    .or_default();
                if let Some(function) = call.function {
                    if let Some(name) = function.name.filter(|name| !name.is_empty()) {
                        entry.name = name;
                    }
                    if let Some(arguments) = function.arguments {
                        entry.arguments.push_str(&arguments);
                    }
                }
            }
        }
        if choice.finish_reason.is_some() {
            chunks.extend(self.flush()?);
        }
        Ok(chunks)
    }

    fn on_content(&mut self, content: String, chunks: &mut Vec<CompletionChunk>) {
        if content.is_empty() {
            return;
        }
        if self.text_open {
            chunks.push(CompletionChunk::TextDelta(content));
            return;
        }
        self.pending_whitespace.push_str(&content);
        if !self.pending_whitespace.trim().is_empty() {
            self.text_open = true;
            chunks.push(CompletionChunk::TextDelta(std::mem::take(
                &mut self.pending_whitespace,
            )));
        }
    }

    /// Closes the open text segment and emits every accumulated tool call.
    fn flush(&mut self) -> Result<Vec<CompletionChunk>, LlmError> {
        let mut chunks = Vec::new();
        self.pending_whitespace.clear();
        if std::mem::take(&mut self.text_open) {
            chunks.push(CompletionChunk::TextDone);
        }
        for (index, call) in std::mem::take(&mut self.calls) {
            if call.name.is_empty() {
                return Err(LlmError::Provider(format!(
                    "streamed tool call at index {index} has no function name"
                )));
            }
            let name = decode_tool_name(&call.name);
            let arguments = decode_arguments(&name, Some(Value::String(call.arguments)))?;
            chunks.push(CompletionChunk::ToolCall { name, arguments });
        }
        Ok(chunks)
    }
}

/// Splits network frames into SSE lines and feeds `data:` payloads to the
/// accumulator. Bytes are buffered and only complete lines are decoded, so a
/// multi-byte character split across two frames survives (`\n` never occurs
/// inside a UTF-8 sequence). Comment lines (`:`), other fields (`event:`,
/// `id:`, …) and blank lines are skipped; `\r\n` is accepted. After an error
/// or `[DONE]` the decoder ignores the rest of the stream.
#[derive(Default)]
struct SseDecoder {
    buffer: Vec<u8>,
    accumulator: SseAccumulator,
    done: bool,
}

type ChunkResults = Vec<Result<CompletionChunk, LlmError>>;

impl SseDecoder {
    fn on_frame(&mut self, frame: &[u8]) -> ChunkResults {
        let mut out = Vec::new();
        self.buffer.extend_from_slice(frame);
        while !self.done {
            let Some(newline) = self.buffer.iter().position(|&byte| byte == b'\n') else {
                break;
            };
            let line: Vec<u8> = self.buffer.drain(..=newline).collect();
            self.on_line(&line, &mut out);
        }
        out
    }

    /// The byte stream ended: decode a last unterminated line, then flush
    /// whatever a missing `finish_reason` / `[DONE]` left open.
    fn on_end(&mut self) -> ChunkResults {
        let mut out = Vec::new();
        if !self.done && !self.buffer.is_empty() {
            let line = std::mem::take(&mut self.buffer);
            self.on_line(&line, &mut out);
        }
        if !self.done {
            self.done = true;
            Self::push(self.accumulator.flush(), &mut out);
        }
        out
    }

    fn on_line(&mut self, raw: &[u8], out: &mut ChunkResults) {
        let line = String::from_utf8_lossy(raw);
        let Some(data) = line.trim_end_matches(['\n', '\r']).strip_prefix("data:") else {
            return;
        };
        let data = data.trim();
        let result = if data == "[DONE]" {
            self.done = true;
            self.accumulator.flush()
        } else {
            self.accumulator.on_data(data)
        };
        if !Self::push(result, out) {
            self.done = true;
        }
    }

    /// Appends a result; `false` when it was an error.
    fn push(result: Result<Vec<CompletionChunk>, LlmError>, out: &mut ChunkResults) -> bool {
        match result {
            Ok(chunks) => {
                out.extend(chunks.into_iter().map(Ok));
                true
            }
            Err(err) => {
                out.push(Err(err));
                false
            }
        }
    }
}

#[async_trait]
impl LlmProvider for OpenAiProvider {
    async fn complete(&self, request: CompletionRequest) -> Result<Vec<CompletionItem>, LlmError> {
        let body = build_body(&self.config, &request, false);
        let response = self.send(&body).await?;
        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|err| LlmError::Provider(err.to_string()))?;
        if !status.is_success() {
            return Err(status_error(status, &text));
        }
        parse_body(&text)
    }

    async fn complete_stream(
        &self,
        request: CompletionRequest,
    ) -> Result<CompletionStream, LlmError> {
        use futures_util::StreamExt;

        let body = build_body(&self.config, &request, true);
        let response = self.send(&body).await?;
        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(status_error(status, &text));
        }

        let mut bytes = response.bytes_stream();
        let stream = async_stream::stream! {
            let mut decoder = SseDecoder::default();
            while let Some(frame) = bytes.next().await {
                let frame = match frame {
                    Ok(frame) => frame,
                    Err(err) => {
                        yield Err(LlmError::Provider(err.to_string()));
                        return;
                    }
                };
                for result in decoder.on_frame(&frame) {
                    let failed = result.is_err();
                    yield result;
                    if failed {
                        return;
                    }
                }
                if decoder.done {
                    return;
                }
            }
            for result in decoder.on_end() {
                yield result;
            }
        };
        Ok(Box::pin(stream))
    }
}

#[cfg(test)]
mod tests {
    use cleverhans_core::seams::{ChatTurn, ToolDef};

    use super::*;

    fn config() -> OpenAiConfig {
        OpenAiConfig::new("http://localhost:8000/v1".to_owned(), "qwen".to_owned())
    }

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
        fn system_turn_stays_in_messages() {
            let body = build_body(&config(), &request(), false);

            assert_eq!(body["messages"][0]["role"], json!("system"));
            assert_eq!(body["model"], json!("qwen"));
            assert_eq!(body["stream"], json!(false));
        }

        #[test]
        fn tools_use_function_shape_with_mangled_names() {
            let body = build_body(&config(), &request(), true);

            assert_eq!(body["tools"][0]["type"], json!("function"));
            assert_eq!(
                body["tools"][0]["function"]["name"],
                json!("transaction__coBuyer__remove")
            );
            assert_eq!(
                body["tools"][0]["function"]["parameters"]["type"],
                json!("object")
            );
        }

        #[test]
        fn tool_turns_become_user_messages() {
            let body = build_body(&config(), &request(), false);

            assert_eq!(body["messages"][2]["role"], json!("user"));
            assert_eq!(
                body["messages"][2]["content"],
                json!("proposal rejected by validation: unknown action")
            );
        }

        #[test]
        fn sends_no_tool_choice_sampling_or_token_cap() {
            let mut no_tools = request();
            no_tools.tools.clear();

            let body = build_body(&config(), &no_tools, false);

            let keys: Vec<_> = body.as_object().expect("object").keys().collect();
            assert_eq!(keys, vec!["messages", "model", "stream"]);
        }
    }

    mod config {
        use super::*;

        #[test]
        fn debug_redacts_the_api_key() {
            let mut config = config();
            config.api_key = Some("sk-secret".to_owned());

            let debug = format!("{config:?}");

            assert!(!debug.contains("sk-secret"), "got {debug}");
            assert!(debug.contains("<redacted>"), "got {debug}");
        }
    }

    mod request {
        use reqwest::header::AUTHORIZATION;

        use super::*;

        fn built(config: OpenAiConfig) -> reqwest::Request {
            OpenAiProvider::new(config)
                .request(&json!({}))
                .expect("builds")
        }

        #[test]
        fn sends_the_key_as_a_bearer_header() {
            let mut config = config();
            config.api_key = Some("sk-local".to_owned());

            let request = built(config);

            assert_eq!(
                request.headers().get(AUTHORIZATION).expect("header"),
                "Bearer sk-local"
            );
        }

        #[test]
        fn sends_no_authorization_header_without_a_key() {
            let request = built(config());

            assert!(request.headers().get(AUTHORIZATION).is_none());
        }

        #[test]
        fn posts_to_chat_completions_with_or_without_a_trailing_slash() {
            for base_url in [
                "http://llm.internal:8000/v1",
                "http://llm.internal:8000/v1/",
            ] {
                let request = built(OpenAiConfig::new(base_url.to_owned(), "m".to_owned()));

                assert_eq!(request.method(), reqwest::Method::POST);
                assert_eq!(
                    request.url().as_str(),
                    "http://llm.internal:8000/v1/chat/completions"
                );
            }
        }
    }

    mod status_error {
        use super::*;

        #[test]
        fn vllm_without_tool_parsing_gets_a_hint_and_keeps_the_body() {
            let body = r#"{"error":{"message":"\"auto\" tool choice requires --enable-auto-tool-choice and --tool-call-parser to be set","type":"BadRequestError","param":null,"code":400}}"#;

            let LlmError::Provider(message) = status_error(StatusCode::BAD_REQUEST, body);

            assert!(message.contains(body), "got {message}");
            assert!(message.contains("hint"), "got {message}");
        }

        #[test]
        fn other_errors_carry_status_and_body_without_a_hint() {
            let LlmError::Provider(message) =
                status_error(StatusCode::BAD_REQUEST, r#"{"error":"bad model"}"#);

            assert!(message.contains("400"), "got {message}");
            assert!(message.contains("bad model"), "got {message}");
            assert!(!message.contains("hint"), "got {message}");
        }
    }

    mod parse_body {
        use super::*;

        fn body(message: &Value) -> String {
            json!({
                "id": "chatcmpl-1",
                "object": "chat.completion",
                "choices": [{"index": 0, "message": message, "finish_reason": "stop"}],
                "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2},
            })
            .to_string()
        }

        #[test]
        fn text_content_becomes_a_text_item() {
            let items = parse_body(&body(
                &json!({"role": "assistant", "content": "Hej, Søren."}),
            ))
            .expect("parseable");

            assert_eq!(items, vec![CompletionItem::Text("Hej, Søren.".to_owned())]);
        }

        #[test]
        fn tool_call_string_arguments_decode_back_to_the_action_id() {
            let items = parse_body(&body(&json!({
                "role": "assistant",
                "content": null,
                "tool_calls": [{"id": "call_1", "type": "function", "function": {
                    "name": "transaction__coBuyer__remove",
                    "arguments": "{\"country\": \"DK\"}",
                }}],
            })))
            .expect("parseable");

            let mut arguments = JsonMap::new();
            arguments.insert("country".to_owned(), json!("DK"));
            assert_eq!(
                items,
                vec![CompletionItem::ToolCall {
                    name: "transaction.coBuyer.remove".to_owned(),
                    arguments,
                }]
            );
        }

        #[test]
        fn empty_argument_string_is_an_empty_object() {
            let items = parse_body(&body(&json!({
                "role": "assistant",
                "content": null,
                "tool_calls": [{"function": {"name": "note__create", "arguments": ""}}],
            })))
            .expect("parseable");

            assert_eq!(
                items,
                vec![CompletionItem::ToolCall {
                    name: "note.create".to_owned(),
                    arguments: JsonMap::new(),
                }]
            );
        }

        #[test]
        fn malformed_arguments_are_an_error_naming_the_tool() {
            for arguments in [json!("{\"country\": "), json!("[1, 2]"), json!({"a": 1})] {
                let result = parse_body(&body(&json!({
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{"function": {"name": "note__create", "arguments": arguments}}],
                })));

                assert!(
                    matches!(&result, Err(LlmError::Provider(msg)) if msg.contains("note.create")),
                    "got {result:?}"
                );
            }
        }

        #[test]
        fn null_content_without_tool_calls_is_no_items() {
            let items = parse_body(&body(&json!({"role": "assistant", "content": null})))
                .expect("parseable");

            assert_eq!(items, vec![]);
        }

        #[test]
        fn whitespace_only_content_is_not_a_message() {
            let items = parse_body(&body(&json!({
                "role": "assistant",
                "content": "\n\n",
                "tool_calls": [{"function": {"name": "note__create", "arguments": "{}"}}],
            })))
            .expect("parseable");

            assert_eq!(
                items,
                vec![CompletionItem::ToolCall {
                    name: "note.create".to_owned(),
                    arguments: JsonMap::new(),
                }]
            );
        }

        #[test]
        fn reasoning_fields_are_not_surfaced() {
            let items = parse_body(&body(&json!({
                "role": "assistant",
                "reasoning": "The user wants a note.",
                "reasoning_content": "The user wants a note.",
                "content": "Done.",
            })))
            .expect("parseable");

            assert_eq!(items, vec![CompletionItem::Text("Done.".to_owned())]);
        }

        #[test]
        fn error_object_is_a_provider_error() {
            let result = parse_body(r#"{"error": {"message": "model not found", "code": 404}}"#);

            assert!(
                matches!(&result, Err(LlmError::Provider(msg)) if msg.contains("model not found")),
                "got {result:?}"
            );
        }
    }

    mod sse_decoder {
        use super::*;

        fn data(chunk: &Value) -> String {
            format!("data: {chunk}\n\n")
        }

        fn delta(delta: &Value) -> String {
            data(&json!({
                "object": "chat.completion.chunk",
                "choices": [{"index": 0, "delta": delta, "finish_reason": null}],
            }))
        }

        fn finish(reason: &str) -> String {
            data(&json!({
                "object": "chat.completion.chunk",
                "choices": [{"index": 0, "delta": {}, "finish_reason": reason}],
            }))
        }

        fn tool_delta(index: usize, name: Option<&str>, arguments: &str) -> String {
            let mut function = json!({"arguments": arguments});
            if let Some(name) = name {
                function["name"] = json!(name);
            }
            let mut call = json!({"index": index, "function": function});
            if name.is_some() {
                call["id"] = json!(format!("call_{index}"));
                call["type"] = json!("function");
            }
            delta(&json!({"tool_calls": [call]}))
        }

        /// Feeds each frame, then ends the stream; panics on an error.
        fn run(frames: &[&[u8]]) -> Vec<CompletionChunk> {
            try_run(frames)
                .into_iter()
                .map(|result| result.expect("valid stream"))
                .collect()
        }

        fn try_run(frames: &[&[u8]]) -> ChunkResults {
            let mut decoder = SseDecoder::default();
            let mut out: ChunkResults = frames
                .iter()
                .flat_map(|frame| decoder.on_frame(frame))
                .collect();
            out.extend(decoder.on_end());
            out
        }

        fn joined(lines: &[String]) -> Vec<u8> {
            lines.concat().into_bytes()
        }

        fn call(name: &str, arguments: &Value) -> CompletionChunk {
            CompletionChunk::ToolCall {
                name: name.to_owned(),
                arguments: arguments.as_object().expect("object").clone(),
            }
        }

        #[test]
        fn streams_text_deltas_and_closes_them_at_finish() {
            let stream = joined(&[
                delta(&json!({"role": "assistant", "content": ""})),
                delta(&json!({"content": "Hel"})),
                delta(&json!({"content": "lo."})),
                finish("stop"),
                "data: [DONE]\n\n".to_owned(),
            ]);

            assert_eq!(
                run(&[&stream[..]]),
                vec![
                    CompletionChunk::TextDelta("Hel".to_owned()),
                    CompletionChunk::TextDelta("lo.".to_owned()),
                    CompletionChunk::TextDone,
                ]
            );
        }

        #[test]
        fn a_fragmented_tool_call_arrives_whole() {
            let stream = joined(&[
                tool_delta(0, Some("transaction__coBuyer__remove"), ""),
                tool_delta(0, None, "{\"coun"),
                tool_delta(0, None, "try\": \"D"),
                tool_delta(0, None, "K\"}"),
                finish("tool_calls"),
            ]);

            assert_eq!(
                run(&[&stream[..]]),
                vec![call(
                    "transaction.coBuyer.remove",
                    &json!({"country": "DK"})
                )]
            );
        }

        #[test]
        fn parallel_tool_calls_arrive_in_index_order() {
            let stream = joined(&[
                tool_delta(1, Some("note__archive"), ""),
                tool_delta(0, Some("note__create"), "{\"title\":"),
                tool_delta(1, None, "{}"),
                tool_delta(0, None, " \"x\"}"),
                finish("tool_calls"),
            ]);

            assert_eq!(
                run(&[&stream[..]]),
                vec![
                    call("note.create", &json!({"title": "x"})),
                    call("note.archive", &json!({})),
                ]
            );
        }

        #[test]
        fn text_then_tool_call_closes_the_text_first() {
            let stream = joined(&[
                delta(&json!({"content": "Proposing."})),
                tool_delta(0, Some("note__create"), "{}"),
                finish("tool_calls"),
            ]);

            assert_eq!(
                run(&[&stream[..]]),
                vec![
                    CompletionChunk::TextDelta("Proposing.".to_owned()),
                    CompletionChunk::TextDone,
                    call("note.create", &json!({})),
                ]
            );
        }

        #[test]
        fn whitespace_before_a_tool_call_is_not_a_message() {
            let stream = joined(&[
                delta(&json!({"content": "\n\n"})),
                tool_delta(0, Some("note__create"), "{}"),
                finish("tool_calls"),
            ]);

            assert_eq!(run(&[&stream[..]]), vec![call("note.create", &json!({}))]);
        }

        #[test]
        fn the_usage_chunk_is_ignored() {
            let stream = joined(&[
                delta(&json!({"content": "Hi"})),
                finish("stop"),
                data(&json!({"object": "chat.completion.chunk", "choices": [],
                             "usage": {"prompt_tokens": 9, "completion_tokens": 1}})),
                "data: [DONE]\n\n".to_owned(),
            ]);

            assert_eq!(
                run(&[&stream[..]]),
                vec![
                    CompletionChunk::TextDelta("Hi".to_owned()),
                    CompletionChunk::TextDone,
                ]
            );
        }

        #[test]
        fn done_flushes_without_a_finish_reason() {
            let stream = joined(&[
                delta(&json!({"content": "Proposing."})),
                tool_delta(0, Some("note__create"), "{}"),
                "data: [DONE]\n\n".to_owned(),
            ]);
            let mut decoder = SseDecoder::default();

            let chunks: Vec<_> = decoder
                .on_frame(&stream)
                .into_iter()
                .map(|result| result.expect("valid"))
                .collect();

            assert!(decoder.done);
            assert_eq!(
                chunks,
                vec![
                    CompletionChunk::TextDelta("Proposing.".to_owned()),
                    CompletionChunk::TextDone,
                    call("note.create", &json!({})),
                ]
            );
            assert!(decoder.on_end().is_empty(), "nothing left after [DONE]");
        }

        #[test]
        fn the_end_of_the_stream_flushes_without_done() {
            let stream = joined(&[tool_delta(0, Some("note__create"), "{}")]);

            assert_eq!(run(&[&stream[..]]), vec![call("note.create", &json!({}))]);
        }

        #[test]
        fn an_error_object_is_a_provider_error() {
            let stream = joined(&[
                delta(&json!({"content": "Hi"})),
                data(&json!({"error": {"message": "Internal server error",
                                       "type": "InternalServerError", "code": 500}})),
                delta(&json!({"content": "never seen"})),
            ]);

            let results = try_run(&[&stream[..]]);

            assert_eq!(results.len(), 2, "got {results:?}");
            assert!(matches!(
                &results[1],
                Err(LlmError::Provider(msg)) if msg.contains("Internal server error")
            ));
        }

        #[test]
        fn a_character_split_across_frames_survives() {
            let stream = joined(&[delta(&json!({"content": "Blåbærgrød"})), finish("stop")]);
            let split = stream
                .windows(2)
                .position(|pair| pair == "å".as_bytes())
                .expect("å in stream")
                + 1;

            let chunks = run(&[&stream[..split], &stream[split..]]);

            assert_eq!(
                chunks,
                vec![
                    CompletionChunk::TextDelta("Blåbærgrød".to_owned()),
                    CompletionChunk::TextDone,
                ]
            );
        }

        #[test]
        fn reasoning_deltas_are_not_surfaced() {
            let stream = joined(&[
                delta(&json!({"reasoning": "The user greets me."})),
                delta(&json!({"reasoning_content": "The user greets me."})),
                delta(&json!({"content": "Hej!"})),
                finish("stop"),
            ]);

            assert_eq!(
                run(&[&stream[..]]),
                vec![
                    CompletionChunk::TextDelta("Hej!".to_owned()),
                    CompletionChunk::TextDone,
                ]
            );
        }

        #[test]
        fn comments_event_lines_and_crlf_are_accepted() {
            let stream = format!(
                ": keep-alive\r\nevent: message\r\n{}\r\n{}",
                delta(&json!({"content": "Hi"})).trim_end(),
                finish("stop").replace('\n', "\r\n"),
            );

            assert_eq!(
                run(&[stream.as_bytes()]),
                vec![
                    CompletionChunk::TextDelta("Hi".to_owned()),
                    CompletionChunk::TextDone,
                ]
            );
        }
    }
}
