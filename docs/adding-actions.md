# Adding an action

The loop is: registry edit → codegen → handlers → UI → evals. One registry
document is the single source for all of it.

## 1. Edit `registry.json`

Add the action (and a new block type only if no existing one fits — most
actions reuse `confirm` or `bulk_preview`):

```json
{ "id": "document.share",
  "description": "Share the selected document with a teammate",
  "params": [
    { "name": "documentId", "type": "string", "source": "context", "required": true },
    { "name": "email", "type": "string", "source": "utterance", "required": true,
      "description": "Email address of the teammate" }
  ],
  "block_type": "confirm", "mutates": true, "authz_key": "documents.share" }
```

Rules that bite:

- Every `source: "context"` param needs a `context_params` entry (param name
  → `route` | `selected_record_id` | `view_type` | `params.<key>` |
  `extensions.<key>`). `schema.context_resolver()` fails at startup naming
  the gap.
- `description` is the intent-matching surface the model sees — write it for
  the model, not for docs.
- Avoid `__` in action ids (LLM tool-name rules mangle `.` to `__`).

Optionally add `display` so the action shows up in the app's command palette
(spec §4.3). The agent ignores it; `palette::match_actions` matches on it:

```json
{ "id": "document.share",
  "description": "…model-facing text as above…",
  "display": {
    "title": "Share document",
    "description": "Send a teammate an access link — you confirm first",
    "keywords": ["share", "send", "access", "teammate"],
    "group": "documents",
    "tags": []
  },
  "params": [ … ] }
```

Write `display.description` for the user (the outcome), not for the model —
the two texts are different surfaces and should stay different.

## 2. Regenerate types

```sh
cleverhans-codegen --schema registry.json --rs src/generated.rs --ts web/src/generated/registry.ts
npx cleverhans-codegen --schema registry.json --ts src/generated/registry.ts   # JS-only teams
```

Add `--check` in CI so a registry edit without regeneration fails the build.

## 3. Bind handlers

```rust
.bind(action_ids::DOCUMENT_SHARE, |action| action
    .handler(typed_handler(|p: DocumentShareParams, user: User| async move {
        share(&p.document_id, &p.email, &user).await   // your normal path
    }))
    .dry_run(typed_dry_run(|p: DocumentShareParams, _: User| async move {
        Ok(DryRunPreview {
            affected_count: 1,
            summary: Some(format!("Share with {}", p.email)),
            ..Default::default()
        })
    }))
    .slots(|params: &JsonMap, _| slots! {
        "title": "Share document",
        "detail": format!("With {}", params["email"]),
    }))
```

Node/Python hosts: add the handler/dry-run/slot-builder entries to the maps
passed to `Agent` — same seams, same names (`dryRuns` / `dry_runs`).

- `mutates: true` ⇒ dry-run required; the preview is what the user confirms
  against, computed under *their* permissions.
- Business declines: `HandlerError::Rejected` / `throw new Rejected(...)` /
  `raise cleverhans_agent.Rejected(...)`.

## 4. Frontend

Reused an existing block type? Nothing to do — the card renders already.
New block type: add a component and register it in your `BlockComponents`
map (`@cleverhans/ui` defaults cover `confirm` and `bulk_preview`).

## 5. Eval cases

Add at least: the happy path, the context-dependent case with *and without*
a selection, and one near-boundary utterance that should decline:

```json
{ "name": "share from detail view", "utterance": "share this with sam@acme.dev",
  "context": { "route": "/documents/doc-1", "selected_record_id": "doc-1" },
  "expected": { "kind": "action", "action_id": "document.share",
                "params": { "email": "sam@acme.dev" } } }
```

Run with `cleverhans::evals::run_suite` (or the demo CLI:
`cargo run -p cleverhans-demo -- eval <cases.json>`).
