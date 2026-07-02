//! The demo's Eddytor-flavored document registry: an in-memory document
//! store with rename/publish/archive/bulk-delete actions. This is the
//! dogfood surface — every seam an app implements is implemented here.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::json;

use cleverhans_core::JsonMap;
use cleverhans_core::envelope::{Context, DryRunPreview};
use cleverhans_core::error::HandlerError;
use cleverhans_core::registry::{
    ActionDef, BlockDef, ParamSource, ParamSpec, Registry, SlotSpec, ValueType,
};
use cleverhans_core::seams::{
    ActionHandler, AuthzDecision, AuthzResolver, ContextParamResolver, DryRunHandler, SlotBuilder,
};

/// The demo principal. Everyone may do everything — this is a dogfood
/// server, not an auth reference.
#[derive(Clone)]
pub struct DemoUser {
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct Document {
    pub id: String,
    pub title: String,
    pub status: DocStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocStatus {
    Draft,
    Published,
    Archived,
}

impl DocStatus {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "draft" => Some(Self::Draft),
            "published" => Some(Self::Published),
            "archived" => Some(Self::Archived),
            _ => None,
        }
    }
}

/// Shared in-memory document store.
#[derive(Clone)]
pub struct Store(Arc<Mutex<Vec<Document>>>);

impl Store {
    pub fn seeded() -> Self {
        let docs = vec![
            Document {
                id: "doc-1".to_owned(),
                title: "Q3 Planning".to_owned(),
                status: DocStatus::Draft,
            },
            Document {
                id: "doc-2".to_owned(),
                title: "Launch Checklist".to_owned(),
                status: DocStatus::Draft,
            },
            Document {
                id: "doc-3".to_owned(),
                title: "Retro Notes".to_owned(),
                status: DocStatus::Published,
            },
        ];
        Self(Arc::new(Mutex::new(docs)))
    }

    fn with<T>(&self, f: impl FnOnce(&mut Vec<Document>) -> T) -> T {
        f(&mut self.0.lock().expect("store lock"))
    }

    fn title_of(&self, id: &str) -> Option<String> {
        self.with(|docs| docs.iter().find(|d| d.id == id).map(|d| d.title.clone()))
    }
}

fn param_str<'a>(params: &'a JsonMap, key: &str) -> Result<&'a str, HandlerError> {
    params
        .get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| HandlerError::Internal(format!("missing param `{key}`")))
}

struct Rename(Store);

#[async_trait]
impl ActionHandler<DemoUser> for Rename {
    async fn execute(
        &self,
        params: &JsonMap,
        _principal: &DemoUser,
    ) -> Result<serde_json::Value, HandlerError> {
        let id = param_str(params, "documentId")?.to_owned();
        let title = param_str(params, "title")?.to_owned();
        self.0.with(|docs| {
            let doc = docs
                .iter_mut()
                .find(|d| d.id == id)
                .ok_or_else(|| HandlerError::Rejected(format!("no document `{id}`")))?;
            doc.title = title.clone();
            Ok(json!({"id": doc.id, "title": doc.title}))
        })
    }
}

struct SetStatus(Store, DocStatus);

#[async_trait]
impl ActionHandler<DemoUser> for SetStatus {
    async fn execute(
        &self,
        params: &JsonMap,
        _principal: &DemoUser,
    ) -> Result<serde_json::Value, HandlerError> {
        let id = param_str(params, "documentId")?.to_owned();
        let status = self.1;
        self.0.with(|docs| {
            let doc = docs
                .iter_mut()
                .find(|d| d.id == id)
                .ok_or_else(|| HandlerError::Rejected(format!("no document `{id}`")))?;
            doc.status = status;
            Ok(json!({"id": doc.id, "status": format!("{status:?}")}))
        })
    }
}

/// Preview for single-document actions: exactly the selected document.
struct OneDocPreview(Store);

#[async_trait]
impl DryRunHandler<DemoUser> for OneDocPreview {
    async fn dry_run(
        &self,
        params: &JsonMap,
        _principal: &DemoUser,
    ) -> Result<DryRunPreview, HandlerError> {
        let id = param_str(params, "documentId")?;
        let title = self
            .0
            .title_of(id)
            .ok_or_else(|| HandlerError::Rejected(format!("no document `{id}`")))?;
        Ok(DryRunPreview {
            affected_count: 1,
            sample_ids: vec![id.to_owned()],
            summary: Some(format!("\u{201c}{title}\u{201d}")),
            extensions: JsonMap::new(),
        })
    }
}

/// The bulk predicate action (spec §4.2): the model names a status, the app
/// resolves which documents match.
struct DeleteByStatus(Store);

impl DeleteByStatus {
    fn matching(&self, params: &JsonMap) -> Result<Vec<Document>, HandlerError> {
        let status = DocStatus::parse(param_str(params, "status")?)
            .ok_or_else(|| HandlerError::Rejected("unknown status".to_owned()))?;
        Ok(self.0.with(|docs| {
            docs.iter()
                .filter(|d| d.status == status)
                .cloned()
                .collect()
        }))
    }
}

#[async_trait]
impl ActionHandler<DemoUser> for DeleteByStatus {
    async fn execute(
        &self,
        params: &JsonMap,
        _principal: &DemoUser,
    ) -> Result<serde_json::Value, HandlerError> {
        let matching: Vec<String> = self.matching(params)?.into_iter().map(|d| d.id).collect();
        self.0
            .with(|docs| docs.retain(|d| !matching.contains(&d.id)));
        Ok(json!({"deleted": matching}))
    }
}

#[async_trait]
impl DryRunHandler<DemoUser> for DeleteByStatus {
    async fn dry_run(
        &self,
        params: &JsonMap,
        _principal: &DemoUser,
    ) -> Result<DryRunPreview, HandlerError> {
        let matching = self.matching(params)?;
        Ok(DryRunPreview {
            affected_count: matching.len() as u64,
            sample_ids: matching.iter().take(5).map(|d| d.id.clone()).collect(),
            summary: Some(format!(
                "Delete {} {} document(s)",
                matching.len(),
                param_str(params, "status").unwrap_or("?")
            )),
            extensions: JsonMap::new(),
        })
    }
}

/// Title/detail slots from the dry-run preview.
struct PreviewSlots(&'static str);

impl SlotBuilder for PreviewSlots {
    fn build(&self, _params: &JsonMap, preview: Option<&DryRunPreview>) -> JsonMap {
        let mut slots = JsonMap::new();
        slots.insert("title".to_owned(), json!(self.0));
        if let Some(summary) = preview.and_then(|p| p.summary.as_deref()) {
            slots.insert("detail".to_owned(), json!(summary));
        }
        slots
    }
}

pub struct AllowAll;

#[async_trait]
impl AuthzResolver<DemoUser> for AllowAll {
    async fn authorize(
        &self,
        _principal: &DemoUser,
        _action_id: &str,
        _params: &JsonMap,
    ) -> AuthzDecision {
        AuthzDecision::Allow
    }
}

/// `documentId` comes from the selected record in context — the model never
/// names a document.
pub struct SelectionResolver;

impl ContextParamResolver for SelectionResolver {
    fn resolve(
        &self,
        _action_id: &str,
        param: &ParamSpec,
        context: &Context,
    ) -> Option<serde_json::Value> {
        (param.name == "documentId")
            .then(|| context.selected_record_id.clone().map(Into::into))
            .flatten()
    }
}

fn document_id_param() -> ParamSpec {
    ParamSpec {
        name: "documentId".to_owned(),
        description: String::new(),
        ty: ValueType::String,
        source: ParamSource::Context,
        required: true,
    }
}

fn single_doc_action(id: &str, description: &str) -> ActionDef {
    ActionDef {
        id: id.to_owned(),
        description: description.to_owned(),
        params: vec![document_id_param()],
        block_type: "confirm".to_owned(),
        mutates: true,
        authz_key: id.to_owned(),
    }
}

/// Builds the demo registry over a shared store.
pub fn build_registry(store: &Store) -> Registry<DemoUser> {
    Registry::builder()
        .block(BlockDef {
            block_type: "confirm".to_owned(),
            slots: vec![
                SlotSpec {
                    name: "title".to_owned(),
                    ty: ValueType::String,
                    required: true,
                },
                SlotSpec {
                    name: "detail".to_owned(),
                    ty: ValueType::String,
                    required: false,
                },
            ],
        })
        .block(BlockDef {
            block_type: "bulk_preview".to_owned(),
            slots: vec![SlotSpec {
                name: "title".to_owned(),
                ty: ValueType::String,
                required: true,
            }],
        })
        .action(
            ActionDef {
                id: "document.rename".to_owned(),
                description: "Rename the currently open document to a new title the user \
                              provides."
                    .to_owned(),
                params: vec![
                    document_id_param(),
                    ParamSpec {
                        name: "title".to_owned(),
                        description: "The new title, exactly as the user wants it".to_owned(),
                        ty: ValueType::String,
                        source: ParamSource::Utterance,
                        required: true,
                    },
                ],
                block_type: "confirm".to_owned(),
                mutates: true,
                authz_key: "document.rename".to_owned(),
            },
            Arc::new(Rename(store.clone())),
            Some(Arc::new(OneDocPreview(store.clone()))),
            Some(Arc::new(PreviewSlots("Rename document"))),
        )
        .action(
            single_doc_action(
                "document.publish",
                "Publish the currently open document so it becomes visible to readers.",
            ),
            Arc::new(SetStatus(store.clone(), DocStatus::Published)),
            Some(Arc::new(OneDocPreview(store.clone()))),
            Some(Arc::new(PreviewSlots("Publish document"))),
        )
        .action(
            single_doc_action(
                "document.archive",
                "Archive the currently open document, hiding it from active lists.",
            ),
            Arc::new(SetStatus(store.clone(), DocStatus::Archived)),
            Some(Arc::new(OneDocPreview(store.clone()))),
            Some(Arc::new(PreviewSlots("Archive document"))),
        )
        .action(
            ActionDef {
                id: "documents.deleteByStatus".to_owned(),
                description: "Delete every document with a given status (draft, published \
                              or archived). Use for bulk cleanup requests like \u{201c}delete \
                              all drafts\u{201d}."
                    .to_owned(),
                params: vec![ParamSpec {
                    name: "status".to_owned(),
                    description: "Which documents to delete".to_owned(),
                    ty: ValueType::StringEnum(vec![
                        "draft".to_owned(),
                        "published".to_owned(),
                        "archived".to_owned(),
                    ]),
                    source: ParamSource::Utterance,
                    required: true,
                }],
                block_type: "bulk_preview".to_owned(),
                mutates: true,
                authz_key: "documents.delete".to_owned(),
            },
            Arc::new(DeleteByStatus(store.clone())),
            Some(Arc::new(DeleteByStatus(store.clone()))),
            Some(Arc::new(PreviewSlots("Bulk delete documents"))),
        )
        .build()
        .expect("demo registry is valid")
}
