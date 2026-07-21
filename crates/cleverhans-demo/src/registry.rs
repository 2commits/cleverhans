//! The demo's Eddytor-flavored document registry: an in-memory document
//! store with rename/publish/archive/bulk-delete actions. This is the
//! dogfood surface — every seam an app implements is implemented here, and
//! every handler style is on display:
//!
//! - typed closure over codegen params ([`typed_handler`]) — rename
//! - plain closure over the raw [`JsonMap`] — publish/archive
//! - trait impls on a struct (stateful, or one impl serving both the
//!   handler and dry-run seams) — bulk delete, single-doc preview
//!
//! Action IDs come from [`crate::generated`], so a typo'd attachment is a
//! compile error, not a runtime `UnknownAttachment`.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::json;

use cleverhans::prelude::*;

use crate::generated::{DocumentRenameParams, action_ids};

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

/// Rename, as a typed closure: the validated params arrive as the codegen
/// struct, so the body never digs JSON by string key.
fn rename_handler(store: &Store) -> Arc<dyn ActionHandler<DemoUser>> {
    let store = store.clone();
    typed_handler(move |params: DocumentRenameParams, _: DemoUser| {
        let store = store.clone();
        async move {
            store.with(|docs| {
                let doc = docs
                    .iter_mut()
                    .find(|d| d.id == params.document_id)
                    .ok_or_else(|| {
                        HandlerError::Rejected(format!("no document `{}`", params.document_id))
                    })?;
                doc.title = params.title.clone();
                Ok(json!({"id": doc.id, "title": doc.title}))
            })
        }
    })
}

/// Publish/archive, as a plain closure over the raw params map.
fn set_status_handler(store: &Store, status: DocStatus) -> Arc<dyn ActionHandler<DemoUser>> {
    let store = store.clone();
    Arc::new(move |params: JsonMap, _: DemoUser| {
        let store = store.clone();
        async move {
            let id = param_str(&params, "documentId")?.to_owned();
            store.with(|docs| {
                let doc = docs
                    .iter_mut()
                    .find(|d| d.id == id)
                    .ok_or_else(|| HandlerError::Rejected(format!("no document `{id}`")))?;
                doc.status = status;
                Ok(json!({"id": doc.id, "status": format!("{status:?}")}))
            })
        }
    })
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
/// resolves which documents match. One struct serves both seams so the
/// dry-run and the execution share the predicate.
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
            // `BulkPreviewBlock` already prints the affected count, so the
            // summary carries only the predicate.
            summary: Some(format!(
                "every {} document",
                param_str(params, "status").unwrap_or("?")
            )),
            extensions: JsonMap::new(),
        })
    }
}

/// The demo's declarative registry document (spec §4). One source, four
/// consumers: this builder, the codegen CLI, the conformance fixtures, and
/// [`crate::generated`].
pub fn demo_schema() -> RegistrySchema {
    RegistrySchema::from_json(include_str!("../registry.json"))
        .expect("demo registry.json is valid")
}

/// `documentId` comes from the selected record in context — the model never
/// names a document. The mapping lives in `registry.json`.
pub fn context_resolver() -> MappedContextResolver {
    demo_schema()
        .context_resolver()
        .expect("demo registry.json maps every context param")
}

/// Builds the demo registry over a shared store: declarative defs from
/// `registry.json`, handlers attached here by generated action ID.
pub fn build_registry(store: &Store) -> Registry<DemoUser> {
    RegistryBuilder::from_schema(demo_schema())
        .bind(action_ids::DOCUMENT_RENAME, |action| {
            action
                .handler(rename_handler(store))
                .dry_run(OneDocPreview(store.clone()))
                // The dry-run summary names the current title; `detail`
                // carries the utterance-sourced new title so the card shows
                // both.
                .slots(|params: &JsonMap, _: Option<&DryRunPreview>| {
                    let new_title = params.get("title").and_then(|v| v.as_str()).unwrap_or("?");
                    slots! {
                        "title": "Rename document",
                        "detail": format!("New title: \u{201c}{new_title}\u{201d}"),
                    }
                })
        })
        .bind(action_ids::DOCUMENT_PUBLISH, |action| {
            action
                .handler(set_status_handler(store, DocStatus::Published))
                .dry_run(OneDocPreview(store.clone()))
                .static_slots(slots! { "title": "Publish document" })
        })
        .bind(action_ids::DOCUMENT_ARCHIVE, |action| {
            action
                .handler(set_status_handler(store, DocStatus::Archived))
                .dry_run(OneDocPreview(store.clone()))
                .static_slots(slots! { "title": "Archive document" })
        })
        .bind(action_ids::DOCUMENTS_DELETE_BY_STATUS, |action| {
            action
                .handler(DeleteByStatus(store.clone()))
                .dry_run(DeleteByStatus(store.clone()))
                .static_slots(slots! { "title": "Bulk delete documents" })
        })
        .build()
        .expect("demo registry is valid")
}
