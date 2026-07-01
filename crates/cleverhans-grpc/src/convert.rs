//! Conversions between the generated proto types and the core envelope.
//!
//! JSON <-> `google.protobuf.Struct` caveat: protobuf `Value` numbers are
//! `f64`, so integers beyond 2^53 lose precision crossing this binding.
//! Actions whose params carry such values should use per-action messages
//! instead (spec §11).

use cleverhans_core::JsonMap;
use cleverhans_core::envelope as core;

use crate::pb;

/// A client message that cannot be mapped onto the core envelope.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ConvertError {
    /// The `oneof event` field was empty.
    #[error("client event has no payload")]
    EmptyEvent,
    /// A required message field was missing.
    #[error("missing field `{0}`")]
    MissingField(&'static str),
}

fn value_to_pb(value: &serde_json::Value) -> prost_types::Value {
    use prost_types::value::Kind;
    let kind = match value {
        serde_json::Value::Null => Kind::NullValue(0),
        serde_json::Value::Bool(b) => Kind::BoolValue(*b),
        // Lossy above 2^53 — see module docs.
        serde_json::Value::Number(n) => Kind::NumberValue(n.as_f64().unwrap_or(f64::NAN)),
        serde_json::Value::String(s) => Kind::StringValue(s.clone()),
        serde_json::Value::Array(items) => Kind::ListValue(prost_types::ListValue {
            values: items.iter().map(value_to_pb).collect(),
        }),
        serde_json::Value::Object(map) => Kind::StructValue(map_to_struct(map)),
    };
    prost_types::Value { kind: Some(kind) }
}

fn pb_to_value(value: prost_types::Value) -> serde_json::Value {
    use prost_types::value::Kind;
    match value.kind {
        None | Some(Kind::NullValue(_)) => serde_json::Value::Null,
        Some(Kind::BoolValue(b)) => serde_json::Value::Bool(b),
        Some(Kind::NumberValue(n)) => serde_json::Number::from_f64(n)
            .map_or(serde_json::Value::Null, serde_json::Value::Number),
        Some(Kind::StringValue(s)) => serde_json::Value::String(s),
        Some(Kind::ListValue(list)) => {
            serde_json::Value::Array(list.values.into_iter().map(pb_to_value).collect())
        }
        Some(Kind::StructValue(s)) => serde_json::Value::Object(struct_to_map(s)),
    }
}

/// Encodes a JSON object as a `google.protobuf.Struct`.
#[must_use]
pub fn map_to_struct(map: &JsonMap) -> prost_types::Struct {
    prost_types::Struct {
        fields: map
            .iter()
            .map(|(k, v)| (k.clone(), value_to_pb(v)))
            .collect(),
    }
}

/// Decodes a `google.protobuf.Struct` into a JSON object.
#[must_use]
pub fn struct_to_map(s: prost_types::Struct) -> JsonMap {
    s.fields
        .into_iter()
        .map(|(k, v)| (k, pb_to_value(v)))
        .collect()
}

fn context_from_pb(context: pb::Context) -> core::Context {
    core::Context {
        route: context.route,
        params: context.params.map(struct_to_map).unwrap_or_default(),
        selected_record_id: context.selected_record_id,
        view_type: context.view_type,
        extensions: context.extensions.map(struct_to_map).unwrap_or_default(),
    }
}

/// Maps a wire client event onto the core envelope.
///
/// # Errors
///
/// [`ConvertError`] for an empty oneof or missing required fields; the
/// binding reports these as recoverable stream errors, never crashes.
pub fn client_event(event: pb::ClientEvent) -> Result<core::ClientEvent, ConvertError> {
    use pb::client_event::Event;
    match event.event.ok_or(ConvertError::EmptyEvent)? {
        Event::Init(init) => Ok(core::ClientEvent::Init {
            spec_version: init.spec_version,
            context: context_from_pb(init.context.ok_or(ConvertError::MissingField("context"))?),
        }),
        Event::ContextUpdate(update) => Ok(core::ClientEvent::ContextUpdate {
            context: context_from_pb(
                update
                    .context
                    .ok_or(ConvertError::MissingField("context"))?,
            ),
            context_seq: update.context_seq,
        }),
        Event::UserMessage(msg) => Ok(core::ClientEvent::UserMessage {
            text: msg.text,
            client_msg_id: msg.client_msg_id,
        }),
        Event::ConfirmAction(confirm) => Ok(core::ClientEvent::ConfirmAction {
            proposal_id: confirm.proposal_id,
        }),
        Event::RejectAction(reject) => Ok(core::ClientEvent::RejectAction {
            proposal_id: reject.proposal_id,
            reason: reject.reason,
        }),
    }
}

fn preview_to_pb(preview: core::DryRunPreview) -> pb::DryRunPreview {
    pb::DryRunPreview {
        affected_count: preview.affected_count,
        sample_ids: preview.sample_ids,
        summary: preview.summary,
        extensions: Some(map_to_struct(&preview.extensions)),
    }
}

/// Maps a core server event onto the wire. Infallible: everything the core
/// emits is representable in the envelope proto by construction.
#[must_use]
pub fn server_event(event: core::ServerEvent) -> pb::ServerEvent {
    use pb::server_event::Event;
    let event = match event {
        core::ServerEvent::ChatMessage { msg_id, text, done } => {
            Event::ChatMessage(pb::ChatMessage { msg_id, text, done })
        }
        core::ServerEvent::ActionProposal(proposal) => Event::ActionProposal(pb::ActionProposal {
            proposal_id: proposal.proposal_id,
            action_id: proposal.action_id,
            params: Some(map_to_struct(&proposal.params)),
            block_type: proposal.block_type,
            slots: Some(map_to_struct(&proposal.slots)),
            preview: proposal.preview.map(preview_to_pb),
            context_seq: proposal.context_seq,
            turn_msg_id: proposal.turn_msg_id,
        }),
        core::ServerEvent::ProposalStateChanged {
            proposal_id,
            state,
            reason,
            result,
        } => Event::ProposalStateChanged(pb::ProposalStateChanged {
            proposal_id,
            state: state.to_string(),
            reason,
            result: result.as_ref().map(value_to_pb),
        }),
        core::ServerEvent::Error {
            code,
            message,
            recoverable,
        } => Event::Error(pb::Error {
            code,
            message,
            recoverable,
        }),
    };
    pb::ServerEvent { event: Some(event) }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    mod struct_round_trip {
        use super::*;

        #[test]
        fn preserves_nested_json() {
            let mut map = JsonMap::new();
            map.insert("country".to_owned(), json!("NO"));
            map.insert("nested".to_owned(), json!({"ids": ["a", "b"], "n": 2.5}));
            map.insert("flag".to_owned(), json!(true));

            let back = struct_to_map(map_to_struct(&map));

            assert_eq!(back, map);
        }
    }

    mod client_event {
        use super::*;

        #[test]
        fn rejects_empty_oneof() {
            let result = client_event(pb::ClientEvent { event: None });

            assert_eq!(result.unwrap_err(), ConvertError::EmptyEvent);
        }

        #[test]
        fn maps_confirm_action() {
            let event = pb::ClientEvent {
                event: Some(pb::client_event::Event::ConfirmAction(pb::ConfirmAction {
                    proposal_id: "prop-1".to_owned(),
                })),
            };

            let result = client_event(event).expect("valid event");

            assert_eq!(
                result,
                core::ClientEvent::ConfirmAction {
                    proposal_id: "prop-1".to_owned()
                }
            );
        }
    }

    mod server_event {
        use cleverhans_core::proposal::ProposalState;

        use super::*;

        #[test]
        fn state_change_uses_spec_state_names() {
            let event = server_event(core::ServerEvent::ProposalStateChanged {
                proposal_id: "prop-1".to_owned(),
                state: ProposalState::Executed,
                reason: None,
                result: Some(json!({"removed": true})),
            });

            let Some(pb::server_event::Event::ProposalStateChanged(changed)) = event.event else {
                panic!("wrong variant: {event:?}");
            };
            assert_eq!(changed.state, "executed");
        }
    }
}
