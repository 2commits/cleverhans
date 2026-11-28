# cleverhans-ws-core

Framework-neutral JSON-frame session loop for the
[CleverHans](https://github.com/2commits/cleverhans) envelope: `run_session`
pumps text frames between any `Stream<Item = String>` / `mpsc::Sender<String>`
pair and one agent session, enforcing init-first, ignoring unknown message
types, and ending the session after a non-recoverable error. It returns a
`SessionExit`: on `Ended(end)` close your socket with `end.close_code()`
(1008) and `end.close_reason()`, and select `SUBPROTOCOL`
(`cleverhans.v0.1`) at the upgrade when the client offers it.

[`cleverhans-ws`](https://crates.io/crates/cleverhans-ws) is the axum adapter
over this crate; use this one directly for actix, warp, tungstenite, or any
other socket you already own. The `FramePump` type serves per-frame hosts
(FFI bindings use it).
