//! Drives one vector against the reference implementation.

use serde_json::Value;

use cleverhans_core::envelope::ClientEvent;
use cleverhans_ws_core::{FrameOutcome, FramePump};

use crate::fixture::{Fixture, Frame, Layer, Step, Vector, VectorPrincipal, build_agent};
use crate::matcher::{Bindings, match_events, substitute};

/// Runs a vector; `Err` carries a report naming the failing step.
///
/// # Errors
///
/// Any expectation mismatch, decode failure, or execution-log divergence.
pub async fn run_vector(fixture: &Fixture, vector: &Vector) -> Result<(), String> {
    let step_result = match vector.layer {
        Layer::Agent => run_agent_layer(fixture, vector).await,
        Layer::Binding => run_binding_layer(fixture, vector).await,
    };
    step_result.map_err(|err| format!("vector `{}`: {err}", vector.name))
}

/// Drops non-normative events before matching: `done: false` chat deltas
/// (spec §6.3 — delta count is implementation detail) and any
/// vector-ignored types.
pub(crate) fn normalize(events: Vec<Value>, vector: &Vector) -> Vec<Value> {
    events
        .into_iter()
        .filter(|event| {
            let ty = event.get("type").and_then(Value::as_str).unwrap_or("");
            if vector.ignore_types.iter().any(|ignored| ignored == ty) {
                return false;
            }
            vector.keep_deltas
                || !(ty == "chat_message" && event.get("done") == Some(&Value::Bool(false)))
        })
        .collect()
}

async fn run_agent_layer(fixture: &Fixture, vector: &Vector) -> Result<(), String> {
    let (agent, log) = build_agent(fixture, vector);
    let mut session = cleverhans_core::agent::Session::new(VectorPrincipal);
    let mut bindings = Bindings::default();
    let mut buffer: Vec<Value> = Vec::new();

    for (index, step) in vector.steps.iter().enumerate() {
        match step {
            Step::Send(payload) => {
                let payload = substitute(payload, &bindings);
                let event: ClientEvent = serde_json::from_value(payload.clone())
                    .map_err(|err| format!("step {index}: send is not a ClientEvent: {err}"))?;
                let emitted = agent.handle(&mut session, event).await;
                for server_event in emitted {
                    buffer.push(
                        serde_json::to_value(&server_event)
                            .map_err(|err| format!("step {index}: encode: {err}"))?,
                    );
                }
            }
            Step::Expect(expected) => {
                let actual = normalize(std::mem::take(&mut buffer), vector);
                match_events(expected, &actual, &mut bindings)
                    .map_err(|err| format!("step {index}: {err}"))?;
            }
        }
    }
    if !normalize(std::mem::take(&mut buffer), vector).is_empty() {
        return Err("trailing events after the last expect".to_owned());
    }
    check_executions(vector, &log)
}

async fn run_binding_layer(fixture: &Fixture, vector: &Vector) -> Result<(), String> {
    let (agent, log) = build_agent(fixture, vector);
    // Drive the same per-frame state machine the FFI bindings use;
    // run_session is a thin loop over it (covered by ws-core's own tests).
    let mut pump = FramePump::new(VectorPrincipal);
    let mut raw_events: Vec<String> = Vec::new();
    for frame in &vector.frames {
        let frame = match frame {
            Frame::Json(value) => value.to_string(),
            Frame::Raw(raw) => raw.clone(),
        };
        let outcome = pump
            .handle_frame(&agent, &frame, &mut |json: String| {
                raw_events.push(json);
                true
            })
            .await;
        if outcome == FrameOutcome::Closed {
            break; // the stream closed; a transport would read no further
        }
    }
    if vector.expect_close != pump.is_closed() {
        return Err(format!(
            "expect_close: expected closed = {}, session closed = {}",
            vector.expect_close,
            pump.is_closed()
        ));
    }
    let mut actual = Vec::new();
    for json in raw_events {
        actual
            .push(serde_json::from_str(&json).map_err(|err| format!("outbound not JSON: {err}"))?);
    }

    let actual = normalize(actual, vector);
    let mut bindings = Bindings::default();
    match_events(&vector.expect, &actual, &mut bindings)?;
    check_executions(vector, &log)
}

fn check_executions(vector: &Vector, log: &crate::fixture::ExecutionLog) -> Result<(), String> {
    let Some(expected) = &vector.executions else {
        return Ok(());
    };
    let actual = log.lock().expect("execution log lock").clone();
    if *expected == actual {
        Ok(())
    } else {
        Err(format!(
            "executions diverge: expected {expected:?}, got {actual:?}"
        ))
    }
}
