//! The validation pipeline (spec §7.1) — runs at propose time and again,
//! unchanged, at confirm time.

use crate::JsonMap;
use crate::envelope::{Context, DryRunPreview};
use crate::error::ValidationFailure;
use crate::registry::{ParamSource, Registry};
use crate::seams::{AuthzDecision, AuthzResolver, ContextParamResolver};

/// A model tool call, before validation: the action it selected plus its
/// utterance-sourced arguments. Context-sourced params are filled by the
/// framework, never taken from here.
#[derive(Debug, Clone, PartialEq)]
pub struct CandidateAction {
    /// The action ID the model referenced.
    pub action_id: String,
    /// Model-emitted arguments.
    pub utterance_params: JsonMap,
}

/// A candidate that passed the full pipeline and may be emitted as a
/// proposal.
#[derive(Debug, Clone, PartialEq)]
pub struct ValidatedAction {
    /// The registered action.
    pub action_id: String,
    /// Fully filled params: context-sourced + validated utterance-sourced.
    pub params: JsonMap,
    /// The action's block type.
    pub block_type: String,
    /// Slot values, already checked against the block schema.
    pub slots: JsonMap,
    /// Dry-run preview; present iff the action mutates.
    pub preview: Option<DryRunPreview>,
}

/// Borrowed view over the seams the pipeline needs.
pub struct Validator<'a, P> {
    registry: &'a Registry<P>,
    authz: &'a dyn AuthzResolver<P>,
    context_params: &'a dyn ContextParamResolver,
}

impl<'a, P> Validator<'a, P> {
    /// Assembles a validator over the given seams.
    pub fn new(
        registry: &'a Registry<P>,
        authz: &'a dyn AuthzResolver<P>,
        context_params: &'a dyn ContextParamResolver,
    ) -> Self {
        Self {
            registry,
            authz,
            context_params,
        }
    }

    /// Runs the full spec §7.1 pipeline: existence, param fill + typecheck,
    /// authorization, dry-run, slot typecheck.
    ///
    /// # Errors
    ///
    /// The first [`ValidationFailure`] encountered; the candidate is then
    /// `invalid` (propose time) or `expired` (confirm time) and must not be
    /// rendered or executed.
    pub async fn validate(
        &self,
        candidate: &CandidateAction,
        context: &Context,
        principal: &P,
    ) -> Result<ValidatedAction, ValidationFailure> {
        let registration = self
            .registry
            .action(&candidate.action_id)
            .ok_or_else(|| ValidationFailure::UnknownAction(candidate.action_id.clone()))?;
        let def = &registration.def;

        let params = self.fill_params(candidate, context, def)?;

        match self.authz.authorize(principal, &def.id, &params).await {
            AuthzDecision::Allow => {}
            AuthzDecision::Deny(reason) => {
                return Err(ValidationFailure::Unauthorized {
                    action_id: def.id.clone(),
                    reason,
                });
            }
        }

        let preview = if def.mutates {
            let Some(dry_run) = registration.dry_run.as_ref() else {
                // Registry build enforces this; guard defensively anyway.
                return Err(ValidationFailure::DryRun(
                    "mutating action has no dry-run handler".to_owned(),
                ));
            };
            Some(
                dry_run
                    .dry_run(&params, principal)
                    .await
                    .map_err(|err| ValidationFailure::DryRun(err.to_string()))?,
            )
        } else {
            None
        };

        let slots = if let Some(builder) = registration.async_slots.as_ref() {
            // §14.9 semantics: a card whose content could not be built is
            // never rendered — fail closed, like dry-run.
            builder
                .build_slots(&params, principal, preview.as_ref())
                .await
                .map_err(|err| ValidationFailure::SlotBuild(err.to_string()))?
        } else {
            registration
                .slot_builder
                .as_ref()
                .map_or_else(JsonMap::new, |builder| {
                    builder.build(&params, preview.as_ref())
                })
        };
        let Some(block) = self.registry.block(&def.block_type) else {
            // Unreachable per registry build invariants; fail closed.
            return Err(ValidationFailure::InvalidSlot {
                slot: String::new(),
                reason: format!("block type `{}` not registered", def.block_type),
            });
        };
        block.check_slots(&slots)?;

        Ok(ValidatedAction {
            action_id: def.id.clone(),
            params,
            block_type: def.block_type.clone(),
            slots,
            preview,
        })
    }

    fn fill_params(
        &self,
        candidate: &CandidateAction,
        context: &Context,
        def: &crate::registry::ActionDef,
    ) -> Result<JsonMap, ValidationFailure> {
        let mut params = JsonMap::new();
        for spec in &def.params {
            match spec.source {
                ParamSource::Context => match self.context_params.resolve(&def.id, spec, context) {
                    Some(value) => {
                        spec.ty.check(&value).map_err(|reason| {
                            ValidationFailure::InvalidParam {
                                param: spec.name.clone(),
                                reason,
                            }
                        })?;
                        params.insert(spec.name.clone(), value);
                    }
                    None if spec.required => {
                        return Err(ValidationFailure::UnresolvedContextParam(spec.name.clone()));
                    }
                    None => {}
                },
                ParamSource::Utterance => match candidate.utterance_params.get(&spec.name) {
                    Some(value) => {
                        spec.ty
                            .check(value)
                            .map_err(|reason| ValidationFailure::InvalidParam {
                                param: spec.name.clone(),
                                reason,
                            })?;
                        params.insert(spec.name.clone(), value.clone());
                    }
                    None if spec.required => {
                        return Err(ValidationFailure::MissingParam(spec.name.clone()));
                    }
                    None => {}
                },
            }
        }
        // Anything the model sent that is not a declared utterance param is
        // rejected — including attempts to write context-sourced params.
        if let Some(unknown) = candidate.utterance_params.keys().find(|key| {
            !def.params
                .iter()
                .any(|p| p.source == ParamSource::Utterance && &p.name == *key)
        }) {
            return Err(ValidationFailure::UnknownParam(unknown.clone()));
        }
        Ok(params)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use serde_json::json;

    use super::*;
    use crate::error::HandlerError;
    use crate::registry::{ActionDef, BlockDef, ParamSpec, SlotSpec, ValueType};
    use crate::seams::{ActionHandler, DryRunHandler, SlotBuilder};

    struct User {
        allowed: bool,
    }

    struct NoopHandler;

    #[async_trait]
    impl ActionHandler<User> for NoopHandler {
        async fn execute(
            &self,
            _params: &JsonMap,
            _principal: &User,
        ) -> Result<serde_json::Value, HandlerError> {
            Ok(serde_json::Value::Null)
        }
    }

    struct OnePreview;

    #[async_trait]
    impl DryRunHandler<User> for OnePreview {
        async fn dry_run(
            &self,
            _params: &JsonMap,
            _principal: &User,
        ) -> Result<DryRunPreview, HandlerError> {
            Ok(DryRunPreview {
                affected_count: 1,
                ..DryRunPreview::default()
            })
        }
    }

    /// Uses the closure blanket impl of [`SlotBuilder`], keeping it covered.
    fn title_slot() -> Arc<dyn SlotBuilder> {
        Arc::new(|_: &JsonMap, _: Option<&DryRunPreview>| {
            let mut slots = JsonMap::new();
            slots.insert("title".to_owned(), json!("Remove record"));
            slots
        })
    }

    struct KeyAuthz;

    #[async_trait]
    impl AuthzResolver<User> for KeyAuthz {
        async fn authorize(
            &self,
            principal: &User,
            _action_id: &str,
            _params: &JsonMap,
        ) -> AuthzDecision {
            if principal.allowed {
                AuthzDecision::Allow
            } else {
                AuthzDecision::Deny("missing permission".to_owned())
            }
        }
    }

    struct SelectionResolver;

    impl ContextParamResolver for SelectionResolver {
        fn resolve(
            &self,
            _action_id: &str,
            param: &ParamSpec,
            context: &Context,
        ) -> Option<serde_json::Value> {
            (param.name == "recordId")
                .then(|| context.selected_record_id.clone().map(Into::into))
                .flatten()
        }
    }

    fn registry() -> Registry<User> {
        Registry::builder()
            .block(BlockDef {
                block_type: "confirm".to_owned(),
                slots: vec![SlotSpec {
                    name: "title".to_owned(),
                    ty: ValueType::String,
                    required: true,
                }],
            })
            .action(
                ActionDef {
                    id: "record.remove".to_owned(),
                    description: "Remove the selected record".to_owned(),
                    params: vec![
                        ParamSpec {
                            name: "recordId".to_owned(),
                            description: String::new(),
                            ty: ValueType::String,
                            source: ParamSource::Context,
                            required: true,
                        },
                        ParamSpec {
                            name: "country".to_owned(),
                            description: "ISO code".to_owned(),
                            ty: ValueType::String,
                            source: ParamSource::Utterance,
                            required: false,
                        },
                    ],
                    block_type: "confirm".to_owned(),
                    mutates: true,
                    authz_key: "record.remove".to_owned(),
                    display: None,
                },
                Arc::new(NoopHandler),
                Some(Arc::new(OnePreview)),
                Some(title_slot()),
            )
            .build()
            .expect("valid registry")
    }

    fn context_with_selection() -> Context {
        Context {
            route: "/records/rec_1".to_owned(),
            selected_record_id: Some("rec_1".to_owned()),
            ..Context::default()
        }
    }

    fn candidate(params: JsonMap) -> CandidateAction {
        CandidateAction {
            action_id: "record.remove".to_owned(),
            utterance_params: params,
        }
    }

    mod validate {
        use super::*;

        #[tokio::test]
        async fn fills_context_param_from_snapshot() {
            let registry = registry();
            let validator = Validator::new(&registry, &KeyAuthz, &SelectionResolver);

            let validated = validator
                .validate(
                    &candidate(JsonMap::new()),
                    &context_with_selection(),
                    &User { allowed: true },
                )
                .await
                .expect("valid candidate");

            assert_eq!(validated.params["recordId"], json!("rec_1"));
        }

        #[tokio::test]
        async fn attaches_dry_run_preview_for_mutating_action() {
            let registry = registry();
            let validator = Validator::new(&registry, &KeyAuthz, &SelectionResolver);

            let validated = validator
                .validate(
                    &candidate(JsonMap::new()),
                    &context_with_selection(),
                    &User { allowed: true },
                )
                .await
                .expect("valid candidate");

            assert_eq!(
                validated.preview.map(|p| p.affected_count),
                Some(1),
                "mutating action must carry a preview"
            );
        }

        #[tokio::test]
        async fn rejects_unknown_action() {
            let registry = registry();
            let validator = Validator::new(&registry, &KeyAuthz, &SelectionResolver);
            let unknown = CandidateAction {
                action_id: "made.up".to_owned(),
                utterance_params: JsonMap::new(),
            };

            let result = validator
                .validate(&unknown, &context_with_selection(), &User { allowed: true })
                .await;

            assert_eq!(
                result.unwrap_err(),
                ValidationFailure::UnknownAction("made.up".to_owned())
            );
        }

        #[tokio::test]
        async fn rejects_model_writing_context_param() {
            let registry = registry();
            let validator = Validator::new(&registry, &KeyAuthz, &SelectionResolver);
            let mut params = JsonMap::new();
            params.insert("recordId".to_owned(), json!("rec_666"));

            let result = validator
                .validate(
                    &candidate(params),
                    &context_with_selection(),
                    &User { allowed: true },
                )
                .await;

            assert_eq!(
                result.unwrap_err(),
                ValidationFailure::UnknownParam("recordId".to_owned())
            );
        }

        #[tokio::test]
        async fn rejects_mistyped_utterance_param() {
            let registry = registry();
            let validator = Validator::new(&registry, &KeyAuthz, &SelectionResolver);
            let mut params = JsonMap::new();
            params.insert("country".to_owned(), json!(42));

            let result = validator
                .validate(
                    &candidate(params),
                    &context_with_selection(),
                    &User { allowed: true },
                )
                .await;

            assert!(matches!(
                result.unwrap_err(),
                ValidationFailure::InvalidParam { param, .. } if param == "country"
            ));
        }

        #[tokio::test]
        async fn rejects_unauthorized_principal() {
            let registry = registry();
            let validator = Validator::new(&registry, &KeyAuthz, &SelectionResolver);

            let result = validator
                .validate(
                    &candidate(JsonMap::new()),
                    &context_with_selection(),
                    &User { allowed: false },
                )
                .await;

            assert!(matches!(
                result.unwrap_err(),
                ValidationFailure::Unauthorized { .. }
            ));
        }

        /// An [`AsyncSlotBuilder`] that phrases the card from the preview —
        /// proving the §9.7 ordering (dry-run first) holds on the async path.
        struct PreviewTitle;

        #[crate::async_trait]
        impl crate::seams::AsyncSlotBuilder<User> for PreviewTitle {
            async fn build_slots(
                &self,
                _params: &JsonMap,
                _principal: &User,
                preview: Option<&DryRunPreview>,
            ) -> Result<JsonMap, HandlerError> {
                let mut slots = JsonMap::new();
                slots.insert(
                    "title".to_owned(),
                    json!(format!(
                        "Affects {}",
                        preview.map_or(0, |p| p.affected_count)
                    )),
                );
                Ok(slots)
            }
        }

        struct FailingSlots;

        #[crate::async_trait]
        impl crate::seams::AsyncSlotBuilder<User> for FailingSlots {
            async fn build_slots(
                &self,
                _params: &JsonMap,
                _principal: &User,
                _preview: Option<&DryRunPreview>,
            ) -> Result<JsonMap, HandlerError> {
                Err(HandlerError::Internal("template service down".to_owned()))
            }
        }

        fn async_slots_registry(
            builder: impl crate::seams::AsyncSlotBuilder<User> + 'static,
        ) -> Registry<User> {
            let schema = crate::schema::RegistrySchema {
                spec_version: crate::SPEC_VERSION.to_owned(),
                blocks: vec![BlockDef {
                    block_type: "confirm".to_owned(),
                    slots: vec![SlotSpec {
                        name: "title".to_owned(),
                        ty: ValueType::String,
                        required: true,
                    }],
                }],
                actions: vec![ActionDef {
                    id: "record.remove".to_owned(),
                    description: "Remove the selected record".to_owned(),
                    params: vec![ParamSpec {
                        name: "recordId".to_owned(),
                        description: String::new(),
                        ty: ValueType::String,
                        source: ParamSource::Context,
                        required: true,
                    }],
                    block_type: "confirm".to_owned(),
                    mutates: true,
                    authz_key: "record.remove".to_owned(),
                    display: None,
                }],
                context_params: Default::default(),
            };
            crate::registry::RegistryBuilder::from_schema(schema)
                .bind("record.remove", |action| {
                    action
                        .handler(NoopHandler)
                        .dry_run(OnePreview)
                        .async_slots(builder)
                })
                .build()
                .expect("valid registry")
        }

        #[tokio::test]
        async fn async_slot_builder_receives_the_preview() {
            let registry = async_slots_registry(PreviewTitle);
            let validator = Validator::new(&registry, &KeyAuthz, &SelectionResolver);

            let validated = validator
                .validate(
                    &candidate(JsonMap::new()),
                    &context_with_selection(),
                    &User { allowed: true },
                )
                .await
                .expect("valid candidate");

            assert_eq!(validated.slots["title"], json!("Affects 1"));
        }

        #[tokio::test]
        async fn async_slot_failure_fails_closed() {
            let registry = async_slots_registry(FailingSlots);
            let validator = Validator::new(&registry, &KeyAuthz, &SelectionResolver);

            let failure = validator
                .validate(
                    &candidate(JsonMap::new()),
                    &context_with_selection(),
                    &User { allowed: true },
                )
                .await
                .expect_err("slot failure must invalidate");

            assert!(
                matches!(failure, ValidationFailure::SlotBuild(_)),
                "got {failure:?}"
            );
            assert!(!failure.is_model_fixable());
        }

        #[tokio::test]
        async fn rejects_when_required_context_param_unresolvable() {
            let registry = registry();
            let validator = Validator::new(&registry, &KeyAuthz, &SelectionResolver);
            let no_selection = Context {
                route: "/records".to_owned(),
                ..Context::default()
            };

            let result = validator
                .validate(
                    &candidate(JsonMap::new()),
                    &no_selection,
                    &User { allowed: true },
                )
                .await;

            assert_eq!(
                result.unwrap_err(),
                ValidationFailure::UnresolvedContextParam("recordId".to_owned())
            );
        }
    }
}
