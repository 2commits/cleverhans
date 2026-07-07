//! PyO3 binding: host a CleverHans agent from Python.
//!
//! Exposed as `cleverhans._native`; the ergonomic surface (dict/path
//! handling, exception classes, JSON parsing) lives in the pure-Python
//! wrapper package under `python/cleverhans/`.
//!
//! Async bridge: every host callback is awaited through
//! `pyo3-async-runtimes`' task-locals mechanism. All tokio spawning that may
//! call back into Python happens in exactly one place (the per-session
//! worker started by [`PySession::handle`]), wrapped in `scope(locals, …)`,
//! so nested seam wrappers find the caller's asyncio loop via
//! `into_future`.

use std::collections::HashMap;
use std::sync::Arc;

use pyo3::exceptions::{PyStopAsyncIteration, PyValueError};
use pyo3::prelude::*;
use pyo3::types::PyDict;
use tokio::sync::{Mutex, mpsc};

use cleverhans_core::JsonMap;
use cleverhans_core::agent::{Agent, AgentConfig};
use cleverhans_core::envelope::{Context, DryRunPreview};
use cleverhans_core::error::{HandlerError, LlmError};
use cleverhans_core::registry::ParamSpec;
use cleverhans_core::schema::RegistrySchema;
use cleverhans_core::seams::{
    ActionHandler, AuthzDecision, AuthzResolver, CompletionItem, CompletionRequest,
    ContextParamResolver, DryRunHandler, LlmProvider, SlotBuilder,
};
use cleverhans_ffi::{FfiPrincipal, LlmSpec, assemble_registry, build_llm};

/// Calls a Python callable and awaits its result if it is awaitable, so
/// hosts may register `async def` and plain `def` interchangeably.
async fn call_host(
    callable: &Py<PyAny>,
    build_args: impl for<'py> FnOnce(Python<'py>) -> PyResult<Vec<Py<PyAny>>> + Send,
) -> PyResult<Py<PyAny>> {
    enum Outcome {
        Ready(Py<PyAny>),
        Pending(std::pin::Pin<Box<dyn Future<Output = PyResult<Py<PyAny>>> + Send>>),
    }
    let outcome = Python::attach(|py| -> PyResult<Outcome> {
        let args = build_args(py)?;
        let out = callable
            .bind(py)
            .call1(pyo3::types::PyTuple::new(py, args)?)?;
        if out.hasattr("__await__")? {
            Ok(Outcome::Pending(Box::pin(
                pyo3_async_runtimes::tokio::into_future(out)?,
            )))
        } else {
            Ok(Outcome::Ready(out.unbind()))
        }
    })?;
    match outcome {
        Outcome::Ready(value) => Ok(value),
        Outcome::Pending(fut) => fut.await,
    }
}

/// Maps a Python exception onto [`HandlerError`]: an instance of the
/// wrapper package's `Rejected` class (including subclasses — the class
/// object is passed in at construction, so identity, not name, decides)
/// becomes a business rejection; anything else is internal.
fn handler_error(err: &PyErr, rejected_class: &Py<PyAny>) -> HandlerError {
    Python::attach(|py| {
        let is_rejected = err
            .value(py)
            .is_instance(rejected_class.bind(py))
            .unwrap_or(false);
        let message = err
            .value(py)
            .str()
            .map(|s| s.to_string())
            .unwrap_or_else(|_| err.to_string());
        if is_rejected {
            HandlerError::Rejected(message)
        } else {
            HandlerError::Internal(message)
        }
    })
}

fn to_py_json<'py>(py: Python<'py>, value: &impl serde::Serialize) -> PyResult<Py<PyAny>> {
    Ok(pythonize::pythonize(py, value)?.unbind())
}

fn from_py_json(value: &Py<PyAny>) -> PyResult<serde_json::Value> {
    Python::attach(|py| Ok(pythonize::depythonize(value.bind(py))?))
}

struct PyHandler {
    callable: Py<PyAny>,
    rejected_class: Arc<Py<PyAny>>,
}

#[async_trait::async_trait]
impl ActionHandler<FfiPrincipal> for PyHandler {
    async fn execute(
        &self,
        params: &JsonMap,
        principal: &FfiPrincipal,
    ) -> Result<serde_json::Value, HandlerError> {
        let (params, principal) = (params.clone(), principal.clone());
        let out = call_host(&self.callable, move |py| {
            Ok(vec![to_py_json(py, &params)?, to_py_json(py, &principal)?])
        })
        .await
        .map_err(|err| handler_error(&err, &self.rejected_class))?;
        from_py_json(&out).map_err(|err| HandlerError::Internal(err.to_string()))
    }
}

struct PyDryRun {
    callable: Py<PyAny>,
    rejected_class: Arc<Py<PyAny>>,
}

#[async_trait::async_trait]
impl DryRunHandler<FfiPrincipal> for PyDryRun {
    async fn dry_run(
        &self,
        params: &JsonMap,
        principal: &FfiPrincipal,
    ) -> Result<DryRunPreview, HandlerError> {
        let (params, principal) = (params.clone(), principal.clone());
        let out = call_host(&self.callable, move |py| {
            Ok(vec![to_py_json(py, &params)?, to_py_json(py, &principal)?])
        })
        .await
        .map_err(|err| handler_error(&err, &self.rejected_class))?;
        let value = from_py_json(&out).map_err(|err| HandlerError::Internal(err.to_string()))?;
        serde_json::from_value(value)
            .map_err(|err| HandlerError::Internal(format!("dry_run returned a bad preview: {err}")))
    }
}

/// Authorization callback: `None`/`True` → allow, `str` → deny with reason;
/// an exception denies (fail closed).
struct PyAuthz(Py<PyAny>);

#[async_trait::async_trait]
impl AuthzResolver<FfiPrincipal> for PyAuthz {
    async fn authorize(
        &self,
        principal: &FfiPrincipal,
        action_id: &str,
        params: &JsonMap,
    ) -> AuthzDecision {
        let (principal, action_id, params) = (principal.clone(), action_id.to_owned(), params.clone());
        let out = call_host(&self.0, move |py| {
            Ok(vec![
                to_py_json(py, &principal)?,
                action_id.into_pyobject(py)?.unbind().into_any(),
                to_py_json(py, &params)?,
            ])
        })
        .await;
        match out {
            Ok(value) => Python::attach(|py| {
                let value = value.bind(py);
                if value.is_none() {
                    return AuthzDecision::Allow;
                }
                if let Ok(flag) = value.extract::<bool>() {
                    return if flag {
                        AuthzDecision::Allow
                    } else {
                        AuthzDecision::Deny("denied".to_owned())
                    };
                }
                match value.extract::<String>() {
                    Ok(reason) => AuthzDecision::Deny(reason),
                    Err(_) => AuthzDecision::Deny("authorize returned a non-decision".to_owned()),
                }
            }),
            Err(err) => AuthzDecision::Deny(format!("authorize raised: {err}")),
        }
    }
}

struct AllowAll;

#[async_trait::async_trait]
impl AuthzResolver<FfiPrincipal> for AllowAll {
    async fn authorize(
        &self,
        _principal: &FfiPrincipal,
        _action_id: &str,
        _params: &JsonMap,
    ) -> AuthzDecision {
        AuthzDecision::Allow
    }
}

/// Synchronous context-param escape hatch: `(action_id, param_name,
/// context_dict) -> value | None`. Must not block. The declarative
/// `context_params` mapping in the registry document covers the common case
/// without any callback.
struct PyContextResolver {
    callable: Py<PyAny>,
    fallback: cleverhans_core::schema::MappedContextResolver,
}

impl ContextParamResolver for PyContextResolver {
    fn resolve(&self, action_id: &str, param: &ParamSpec, context: &Context) -> Option<serde_json::Value> {
        let resolved = Python::attach(|py| -> PyResult<Option<serde_json::Value>> {
            let out = self.callable.bind(py).call1((
                action_id,
                param.name.as_str(),
                pythonize::pythonize(py, context)?,
            ))?;
            if out.is_none() {
                Ok(None)
            } else {
                Ok(Some(pythonize::depythonize(&out)?))
            }
        });
        match resolved {
            Ok(Some(value)) => Some(value),
            Ok(None) => self.fallback.resolve(action_id, param, context),
            Err(_) => None, // fail closed: unresolvable, never guessed
        }
    }
}

/// Synchronous slot-builder escape hatch: `(params, preview | None) -> dict`.
struct PySlots(Py<PyAny>);

impl SlotBuilder for PySlots {
    fn build(&self, params: &JsonMap, preview: Option<&DryRunPreview>) -> JsonMap {
        Python::attach(|py| -> PyResult<JsonMap> {
            let preview = match preview {
                Some(preview) => pythonize::pythonize(py, preview)?.unbind(),
                None => py.None(),
            };
            let out = self
                .0
                .bind(py)
                .call1((pythonize::pythonize(py, params)?, preview))?;
            Ok(pythonize::depythonize(&out)?)
        })
        .unwrap_or_default() // schema check downstream reports empty slots
    }
}

/// Custom LLM provider callback: `(request_dict) -> list[{"text": ...} |
/// {"tool_call": {"name", "arguments"}}]`, sync or async. Non-streaming in
/// v1; the core's default adapter chunks it.
struct PyLlm(Py<PyAny>);

#[async_trait::async_trait]
impl LlmProvider for PyLlm {
    async fn complete(&self, request: CompletionRequest) -> Result<Vec<CompletionItem>, LlmError> {
        let out = call_host(&self.0, move |py| Ok(vec![to_py_json(py, &request)?]))
            .await
            .map_err(|err| LlmError::Provider(err.to_string()))?;
        let value = from_py_json(&out).map_err(|err| LlmError::Provider(err.to_string()))?;
        let items: Vec<cleverhans_ffi::LlmItem> = serde_json::from_value(value)
            .map_err(|err| LlmError::Provider(format!("bad llm items: {err}")))?;
        Ok(items.into_iter().map(Into::into).collect())
    }
}

fn value_err(err: impl std::fmt::Display) -> PyErr {
    PyValueError::new_err(err.to_string())
}

/// The agent, stateless and shared across sessions.
#[pyclass(frozen)]
struct PyAgent {
    inner: Arc<Agent<FfiPrincipal>>,
}

fn collect_callables(map: Option<&Bound<'_, PyDict>>) -> PyResult<HashMap<String, Py<PyAny>>> {
    let Some(map) = map else {
        return Ok(HashMap::new());
    };
    map.iter()
        .map(|(key, value)| Ok((key.extract::<String>()?, value.unbind())))
        .collect()
}

#[pymethods]
impl PyAgent {
    #[new]
    #[pyo3(signature = (registry_json, handlers, rejected_class, dry_runs=None,
                        slot_builders=None, authorize=None, llm_spec_json=None,
                        llm_callable=None, resolve_context_param=None, config_json=None))]
    #[allow(clippy::too_many_arguments)] // ergonomic keyword surface, normalized by the wrapper
    fn new(
        registry_json: &str,
        handlers: &Bound<'_, PyDict>,
        rejected_class: Py<PyAny>,
        dry_runs: Option<&Bound<'_, PyDict>>,
        slot_builders: Option<&Bound<'_, PyDict>>,
        authorize: Option<Py<PyAny>>,
        llm_spec_json: Option<&str>,
        llm_callable: Option<Py<PyAny>>,
        resolve_context_param: Option<Py<PyAny>>,
        config_json: Option<&str>,
    ) -> PyResult<Self> {
        let schema = RegistrySchema::from_json(registry_json).map_err(value_err)?;
        let rejected_class = Arc::new(rejected_class);

        let handlers: HashMap<String, Arc<dyn ActionHandler<FfiPrincipal>>> =
            collect_callables(Some(handlers))?
                .into_iter()
                .map(|(id, callable)| {
                    let handler = PyHandler {
                        callable,
                        rejected_class: Arc::clone(&rejected_class),
                    };
                    (id, Arc::new(handler) as _)
                })
                .collect();
        let dry_runs: HashMap<String, Arc<dyn DryRunHandler<FfiPrincipal>>> =
            collect_callables(dry_runs)?
                .into_iter()
                .map(|(id, callable)| {
                    let dry_run = PyDryRun {
                        callable,
                        rejected_class: Arc::clone(&rejected_class),
                    };
                    (id, Arc::new(dry_run) as _)
                })
                .collect();
        let slot_builders: HashMap<String, Arc<dyn SlotBuilder>> = collect_callables(slot_builders)?
            .into_iter()
            .map(|(id, callable)| (id, Arc::new(PySlots(callable)) as _))
            .collect();

        let context_resolver: Arc<dyn ContextParamResolver> = match resolve_context_param {
            Some(callable) => Arc::new(PyContextResolver {
                callable,
                fallback: schema.context_resolver(),
            }),
            None => Arc::new(schema.context_resolver()),
        };
        let authz: Arc<dyn AuthzResolver<FfiPrincipal>> = match authorize {
            Some(callable) => Arc::new(PyAuthz(callable)),
            None => Arc::new(AllowAll),
        };
        let llm: Arc<dyn LlmProvider> = match (llm_spec_json, llm_callable) {
            (Some(spec), None) => {
                let spec: LlmSpec = serde_json::from_str(spec).map_err(value_err)?;
                build_llm(spec).map_err(value_err)?
            }
            (None, Some(callable)) => Arc::new(PyLlm(callable)),
            _ => return Err(value_err("provide exactly one of llm spec or llm callable")),
        };
        let config: AgentConfig = match config_json {
            Some(json) => cleverhans_ffi::parse_agent_config(json).map_err(value_err)?,
            None => AgentConfig::default(),
        };

        let registry = assemble_registry(schema, handlers, dry_runs, slot_builders)
            .map_err(value_err)?;
        Ok(Self {
            inner: Arc::new(Agent::with_config(
                Arc::new(registry),
                llm,
                authz,
                context_resolver,
                config,
            )),
        })
    }

    /// Opens a session bound to a principal (any JSON-able identity blob).
    fn session(&self, principal: Py<PyAny>) -> PyResult<PySession> {
        let principal = from_py_json(&principal)?;
        Ok(PySession {
            agent: Arc::clone(&self.inner),
            principal,
            worker: std::sync::Mutex::new(None),
            closed: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        })
    }
}

/// One inbound frame queued for the session worker.
struct Turn {
    frame: String,
    events: mpsc::UnboundedSender<String>,
}

/// One envelope session; frames in, frames out. A single worker task owns
/// the [`FramePump`] and processes turns strictly in `handle()` call order
/// (an unbounded FIFO channel), so concurrent callers can never reorder
/// frames — the property the protocol's init-first rule depends on.
#[pyclass(frozen)]
struct PySession {
    agent: Arc<Agent<FfiPrincipal>>,
    principal: FfiPrincipal,
    /// Lazily-started worker; task-locals are captured from the first
    /// `handle()` call, so one session belongs to one asyncio event loop.
    worker: std::sync::Mutex<Option<mpsc::UnboundedSender<Turn>>>,
    closed: Arc<std::sync::atomic::AtomicBool>,
}

#[pymethods]
impl PySession {
    /// Handles one inbound JSON frame, returning an async iterator of
    /// outbound JSON frames (strings). Must be called from a running
    /// asyncio event loop. Turns are processed in call order; a turn whose
    /// iterator is abandoned still runs to completion (its side effects
    /// happen), matching stream-transport semantics.
    fn handle(&self, py: Python<'_>, frame: String) -> PyResult<PyEventStream> {
        let mut worker = self.worker.lock().expect("worker lock");
        let turns = match worker.as_ref() {
            Some(turns) if !turns.is_closed() => turns.clone(),
            _ => {
                let locals = pyo3_async_runtimes::tokio::get_current_locals(py)?;
                let (turns_tx, mut turns_rx) = mpsc::unbounded_channel::<Turn>();
                let agent = Arc::clone(&self.agent);
                let principal = self.principal.clone();
                let closed = Arc::clone(&self.closed);
                pyo3_async_runtimes::tokio::get_runtime().spawn(
                    pyo3_async_runtimes::tokio::scope(locals, async move {
                        let mut pump = cleverhans_ffi::FramePump::new(principal);
                        while let Some(turn) = turns_rx.recv().await {
                            let mut events = turn.events;
                            let outcome = pump
                                .handle_frame(&agent, &turn.frame, &mut events)
                                .await;
                            if outcome == cleverhans_ffi::FrameOutcome::Closed {
                                closed.store(true, std::sync::atomic::Ordering::SeqCst);
                            }
                        }
                    }),
                );
                *worker = Some(turns_tx.clone());
                turns_tx
            }
        };
        let (tx, rx) = mpsc::unbounded_channel::<String>();
        turns
            .send(Turn { frame, events: tx })
            .map_err(|_| value_err("session worker stopped"))?;
        Ok(PyEventStream {
            rx: Arc::new(Mutex::new(rx)),
        })
    }

    /// Whether an init-first violation has closed the session (spec §6.1).
    /// Once true, further frames yield no events; close your transport.
    #[getter]
    fn closed(&self) -> bool {
        self.closed.load(std::sync::atomic::Ordering::SeqCst)
    }
}

/// Async iterator over one turn's outbound frames.
#[pyclass(frozen)]
struct PyEventStream {
    rx: Arc<Mutex<mpsc::UnboundedReceiver<String>>>,
}

#[pymethods]
impl PyEventStream {
    fn __aiter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    fn __anext__<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let rx = Arc::clone(&self.rx);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            rx.lock()
                .await
                .recv()
                .await
                .ok_or_else(|| PyStopAsyncIteration::new_err(()))
        })
    }
}

#[pymodule]
fn _native(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<PyAgent>()?;
    module.add_class::<PySession>()?;
    module.add_class::<PyEventStream>()?;
    Ok(())
}
