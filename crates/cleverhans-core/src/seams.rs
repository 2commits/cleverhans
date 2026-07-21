//! Seam interfaces — what the host application implements (spec §9).
//!
//! The framework owns the envelope, state machine, validation pipeline, and
//! agent loop; everything here is plugged in by the app. All traits are
//! object-safe so registrations can be stored heterogeneously; generic over
//! the app's principal type `P` so the framework never invents its own
//! identity model.

use std::future::Future;
use std::marker::PhantomData;
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use futures_core::Stream;
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::JsonMap;
use crate::envelope::{Context, DryRunPreview};
use crate::error::{HandlerError, LlmError};
use crate::registry::ParamSpec;

/// The app's normal execution path for one action (spec §9.2). Invoked only
/// after user confirmation and confirm-time revalidation, always under the
/// authenticated principal.
#[async_trait]
pub trait ActionHandler<P>: Send + Sync {
    /// Executes the action with fully validated params.
    ///
    /// # Errors
    ///
    /// [`HandlerError`] moves the proposal to `failed`; nothing retries
    /// automatically.
    async fn execute(
        &self,
        params: &JsonMap,
        principal: &P,
    ) -> Result<serde_json::Value, HandlerError>;
}

/// Async closures are action handlers, so stateless registrations stay
/// inline — mirroring the [`SlotBuilder`] blanket impl. The closure takes
/// owned params and principal (`P: Clone`) so its future is self-contained.
///
/// ```
/// use std::sync::Arc;
/// use cleverhans_core::JsonMap;
/// use cleverhans_core::seams::ActionHandler;
///
/// #[derive(Clone)]
/// struct User;
///
/// let handler: Arc<dyn ActionHandler<User>> =
///     Arc::new(|params: JsonMap, _user: User| async move {
///         Ok(serde_json::Value::Object(params))
///     });
/// ```
#[async_trait]
impl<P, F, Fut> ActionHandler<P> for F
where
    P: Clone + Send + Sync,
    F: Fn(JsonMap, P) -> Fut + Send + Sync,
    Fut: Future<Output = Result<serde_json::Value, HandlerError>> + Send,
{
    async fn execute(
        &self,
        params: &JsonMap,
        principal: &P,
    ) -> Result<serde_json::Value, HandlerError> {
        self(params.clone(), principal.clone()).await
    }
}

/// Side-effect-free preview of a mutating action (spec §7.2), computed under
/// the principal's own data-access rules so it is permission-correct.
#[async_trait]
pub trait DryRunHandler<P>: Send + Sync {
    /// Computes what `execute` would do, without doing it.
    ///
    /// # Errors
    ///
    /// [`HandlerError`] makes the candidate proposal invalid (propose time)
    /// or expired (confirm time).
    async fn dry_run(&self, params: &JsonMap, principal: &P)
    -> Result<DryRunPreview, HandlerError>;
}

/// Async closures are dry-run handlers too; see the [`ActionHandler`]
/// blanket impl. The two blankets never collide — the future's output type
/// picks the trait.
#[async_trait]
impl<P, F, Fut> DryRunHandler<P> for F
where
    P: Clone + Send + Sync,
    F: Fn(JsonMap, P) -> Fut + Send + Sync,
    Fut: Future<Output = Result<DryRunPreview, HandlerError>> + Send,
{
    async fn dry_run(
        &self,
        params: &JsonMap,
        principal: &P,
    ) -> Result<DryRunPreview, HandlerError> {
        self(params.clone(), principal.clone()).await
    }
}

/// An `Arc`'d handler is a handler, so helpers that return
/// `Arc<dyn ActionHandler<P>>` (e.g. [`typed_handler`]) and bare
/// closures/structs pass through the same `impl ActionHandler` surface
/// (see [`RegistryBuilder::bind`](crate::registry::RegistryBuilder::bind)).
#[async_trait]
impl<P: Send + Sync> ActionHandler<P> for Arc<dyn ActionHandler<P>> {
    async fn execute(
        &self,
        params: &JsonMap,
        principal: &P,
    ) -> Result<serde_json::Value, HandlerError> {
        (**self).execute(params, principal).await
    }
}

/// See the [`ActionHandler`] impl for `Arc<dyn ActionHandler<P>>`.
#[async_trait]
impl<P: Send + Sync> DryRunHandler<P> for Arc<dyn DryRunHandler<P>> {
    async fn dry_run(
        &self,
        params: &JsonMap,
        principal: &P,
    ) -> Result<DryRunPreview, HandlerError> {
        (**self).dry_run(params, principal).await
    }
}

fn parse_params<T: DeserializeOwned>(params: &JsonMap) -> Result<T, HandlerError> {
    serde_json::from_value(serde_json::Value::Object(params.clone())).map_err(|err| {
        HandlerError::Internal(format!(
            "validated params did not match the handler's params type \
             (registry/codegen drift?): {err}"
        ))
    })
}

/// Wraps a closure over a typed params struct (e.g. codegen output) into an
/// [`ActionHandler`]: the validated [`JsonMap`] is deserialized into `T`
/// before the closure runs, so handler bodies never dig params out of JSON
/// by string key.
///
/// ```
/// use cleverhans_core::seams::typed_handler;
///
/// #[derive(Clone)]
/// struct User;
///
/// #[derive(serde::Deserialize)]
/// struct RenameParams {
///     title: String,
/// }
///
/// let handler = typed_handler(|params: RenameParams, _user: User| async move {
///     Ok(serde_json::json!({ "title": params.title }))
/// });
/// ```
pub fn typed_handler<P, T, F, Fut>(f: F) -> Arc<dyn ActionHandler<P>>
where
    P: Clone + Send + Sync + 'static,
    T: DeserializeOwned + Send + 'static,
    F: Fn(T, P) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<serde_json::Value, HandlerError>> + Send + 'static,
{
    struct Typed<F, T> {
        f: F,
        _params: PhantomData<fn() -> T>,
    }

    #[async_trait]
    impl<P, T, F, Fut> ActionHandler<P> for Typed<F, T>
    where
        P: Clone + Send + Sync,
        T: DeserializeOwned + Send,
        F: Fn(T, P) -> Fut + Send + Sync,
        Fut: Future<Output = Result<serde_json::Value, HandlerError>> + Send,
    {
        async fn execute(
            &self,
            params: &JsonMap,
            principal: &P,
        ) -> Result<serde_json::Value, HandlerError> {
            (self.f)(parse_params(params)?, principal.clone()).await
        }
    }

    Arc::new(Typed {
        f,
        _params: PhantomData,
    })
}

/// The [`DryRunHandler`] counterpart of [`typed_handler`].
pub fn typed_dry_run<P, T, F, Fut>(f: F) -> Arc<dyn DryRunHandler<P>>
where
    P: Clone + Send + Sync + 'static,
    T: DeserializeOwned + Send + 'static,
    F: Fn(T, P) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<DryRunPreview, HandlerError>> + Send + 'static,
{
    struct Typed<F, T> {
        f: F,
        _params: PhantomData<fn() -> T>,
    }

    #[async_trait]
    impl<P, T, F, Fut> DryRunHandler<P> for Typed<F, T>
    where
        P: Clone + Send + Sync,
        T: DeserializeOwned + Send,
        F: Fn(T, P) -> Fut + Send + Sync,
        Fut: Future<Output = Result<DryRunPreview, HandlerError>> + Send,
    {
        async fn dry_run(
            &self,
            params: &JsonMap,
            principal: &P,
        ) -> Result<DryRunPreview, HandlerError> {
            (self.f)(parse_params(params)?, principal.clone()).await
        }
    }

    Arc::new(Typed {
        f,
        _params: PhantomData,
    })
}

/// Authorization decision from the app's permission system.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthzDecision {
    /// The principal may perform the action.
    Allow,
    /// The principal may not; the reason is surfaced in validation failures.
    Deny(String),
}

/// The app's permission system, treated as opaque by the framework
/// (spec §9.3). Called at propose time *and* again at confirm time.
#[async_trait]
pub trait AuthzResolver<P>: Send + Sync {
    /// Decides whether `principal` may perform `action_id` with `params`.
    async fn authorize(&self, principal: &P, action_id: &str, params: &JsonMap) -> AuthzDecision;
}

/// Async closures are authz resolvers, mirroring the [`ActionHandler`]
/// blanket impl — apps bridging an existing permission check don't need a
/// trait impl.
///
/// ```
/// use std::sync::Arc;
/// use cleverhans_core::JsonMap;
/// use cleverhans_core::seams::{AuthzDecision, AuthzResolver};
///
/// #[derive(Clone)]
/// struct User {
///     admin: bool,
/// }
///
/// let authz: Arc<dyn AuthzResolver<User>> =
///     Arc::new(|user: User, action_id: String, _params: JsonMap| async move {
///         if user.admin || !action_id.starts_with("admin.") {
///             AuthzDecision::Allow
///         } else {
///             AuthzDecision::Deny("admins only".to_owned())
///         }
///     });
/// ```
#[async_trait]
impl<P, F, Fut> AuthzResolver<P> for F
where
    P: Clone + Send + Sync,
    F: Fn(P, String, JsonMap) -> Fut + Send + Sync,
    Fut: Future<Output = AuthzDecision> + Send,
{
    async fn authorize(&self, principal: &P, action_id: &str, params: &JsonMap) -> AuthzDecision {
        self(principal.clone(), action_id.to_owned(), params.clone()).await
    }
}

/// An [`AuthzResolver`] that allows every action, for demos, tests, and
/// apps whose transport-level auth is the whole permission model. Production
/// apps with per-action permissions implement the trait (or pass a closure)
/// over their real permission system.
pub struct AllowAll;

#[async_trait]
impl<P: Send + Sync> AuthzResolver<P> for AllowAll {
    async fn authorize(&self, _: &P, _: &str, _: &JsonMap) -> AuthzDecision {
        AuthzDecision::Allow
    }
}

/// Extracts context-sourced param values from the current context snapshot
/// (spec §9.5). Only the framework calls this — the model never writes
/// context-sourced params.
pub trait ContextParamResolver: Send + Sync {
    /// Returns the value for `param` of `action_id` given `context`, or
    /// `None` if the context cannot supply it.
    fn resolve(
        &self,
        action_id: &str,
        param: &ParamSpec,
        context: &Context,
    ) -> Option<serde_json::Value>;
}

/// Builds slot values for a proposal's block from validated params and the
/// dry-run preview. App code, not model output: even slot *content* comes
/// from the app in the reference implementation, keeping the rendered UI
/// fully closed-vocabulary.
///
/// Three ways to register one, in order of reach:
///
/// - fixed card content: [`static_slots`] with the [`slots!`](crate::slots)
///   macro
/// - content from params/preview: any closure, via the blanket impl below
/// - content needing owned state (store handles, etc.): implement the trait
///
/// ```
/// use std::sync::Arc;
/// use cleverhans_core::envelope::DryRunPreview;
/// use cleverhans_core::seams::{SlotBuilder, static_slots};
/// use cleverhans_core::{JsonMap, slots};
///
/// // Fixed:
/// let publish = static_slots(slots! { "title": "Publish document" });
///
/// // Param-aware:
/// let rename: Arc<dyn SlotBuilder> =
///     Arc::new(|params: &JsonMap, _: Option<&DryRunPreview>| {
///         slots! {
///             "title": "Rename document",
///             "detail": format!("New title: {}", params["title"]),
///         }
///     });
/// ```
pub trait SlotBuilder: Send + Sync {
    /// Produces the slot map validated against the block's slot schema.
    fn build(&self, params: &JsonMap, preview: Option<&DryRunPreview>) -> JsonMap;
}

/// Closures are slot builders, so per-action registrations stay inline.
impl<F> SlotBuilder for F
where
    F: Fn(&JsonMap, Option<&DryRunPreview>) -> JsonMap + Send + Sync,
{
    fn build(&self, params: &JsonMap, preview: Option<&DryRunPreview>) -> JsonMap {
        self(params, preview)
    }
}

/// An `Arc`'d slot builder is a slot builder; see the matching
/// [`ActionHandler`] impl for `Arc<dyn ActionHandler<P>>`.
impl SlotBuilder for Arc<dyn SlotBuilder> {
    fn build(&self, params: &JsonMap, preview: Option<&DryRunPreview>) -> JsonMap {
        (**self).build(params, preview)
    }
}

/// A [`SlotBuilder`] that emits the same slots for every proposal — for
/// actions whose card content is fixed and the dry-run summary says the
/// rest. See [`SlotBuilder`] for the full menu.
#[must_use]
pub fn static_slots(slots: JsonMap) -> Arc<dyn SlotBuilder> {
    Arc::new(move |_: &JsonMap, _: Option<&DryRunPreview>| slots.clone())
}

/// One entry in the model-facing tool list, derived from the registry.
/// Exposes only utterance-sourced params (spec §4.1).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ToolDef {
    /// The action ID.
    pub name: String,
    /// The registry description — the intent-matching surface.
    pub description: String,
    /// JSON schema of utterance-sourced params.
    pub parameters: serde_json::Value,
}

/// Who authored a chat turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatRole {
    /// Framework- or app-supplied instructions.
    System,
    /// The human.
    User,
    /// The model.
    Assistant,
    /// Tool/action outcome fed back to the model.
    Tool,
}

/// One turn of conversation history.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ChatTurn {
    /// Author.
    pub role: ChatRole,
    /// Content.
    pub content: String,
}

/// A single completion request to the model.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CompletionRequest {
    /// Conversation so far.
    pub messages: Vec<ChatTurn>,
    /// The registry as tool definitions.
    pub tools: Vec<ToolDef>,
}

/// One item of model output.
#[derive(Debug, Clone, PartialEq)]
pub enum CompletionItem {
    /// Assistant prose.
    Text(String),
    /// A structured selection of one registered action.
    ToolCall {
        /// The action ID the model selected.
        name: String,
        /// Utterance-sourced arguments.
        arguments: JsonMap,
    },
}

/// The single seam through which model access flows (spec §9.4): BYO API
/// key, fully local, or an internal gateway — the framework does not care.
#[async_trait]
pub trait LlmProvider: Send + Sync {
    /// Runs one completion over the conversation with the registry as tools.
    ///
    /// # Errors
    ///
    /// [`LlmError`] is surfaced to the client as a recoverable stream error.
    async fn complete(&self, request: CompletionRequest) -> Result<Vec<CompletionItem>, LlmError>;

    /// Streaming variant of [`LlmProvider::complete`]. The default adapts
    /// the non-streaming call into one-shot chunks, so providers only need
    /// to override this when they can stream natively.
    ///
    /// # Errors
    ///
    /// [`LlmError`] on request failure; mid-stream failures arrive as `Err`
    /// items on the stream itself.
    async fn complete_stream(
        &self,
        request: CompletionRequest,
    ) -> Result<CompletionStream, LlmError> {
        let chunks: Vec<Result<CompletionChunk, LlmError>> = self
            .complete(request)
            .await?
            .into_iter()
            .flat_map(|item| match item {
                CompletionItem::Text(text) => {
                    vec![CompletionChunk::TextDelta(text), CompletionChunk::TextDone]
                }
                CompletionItem::ToolCall { name, arguments } => {
                    vec![CompletionChunk::ToolCall { name, arguments }]
                }
            })
            .map(Ok)
            .collect();
        Ok(Box::pin(futures_util::stream::iter(chunks)))
    }
}

/// One increment of streamed model output.
#[derive(Debug, Clone, PartialEq)]
pub enum CompletionChunk {
    /// An incremental fragment of assistant prose.
    TextDelta(String),
    /// The current text segment is complete; the deltas since the last
    /// boundary form one chat message.
    TextDone,
    /// A complete structured selection of one registered action. Tool calls
    /// are never fragmented — arguments arrive whole.
    ToolCall {
        /// The action ID the model selected.
        name: String,
        /// Utterance-sourced arguments.
        arguments: JsonMap,
    },
}

/// Streamed completion output.
pub type CompletionStream = Pin<Box<dyn Stream<Item = Result<CompletionChunk, LlmError>> + Send>>;
