# cleverhans-ws-core

Framework-neutral JSON-frame session loop for the
[CleverHans](https://github.com/2commits/cleverhans) envelope: `run_session`
pumps text frames between any `Stream<Item = String>` / `mpsc::Sender<String>`
pair and one agent session, enforcing init-first and close-on-violation.

[`cleverhans-ws`](https://crates.io/crates/cleverhans-ws) is the axum adapter
over this crate; use this one directly for actix, warp, tungstenite, or any
other socket you already own. The `FramePump` type serves per-frame hosts
(FFI bindings use it).
