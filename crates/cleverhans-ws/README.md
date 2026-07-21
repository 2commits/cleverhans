# cleverhans-ws

WebSocket + JSON transport binding for the
[CleverHans](https://github.com/nordalf/cleverhans) envelope, as an axum
router. Two mounts:

- `agent_router_from_extension("/agent", agent)` — your existing tower/axum
  auth middleware already inserts your user type as an `Extension`; zero
  extra glue.
- `agent_router("/agent", agent, extractor)` — implement `PrincipalExtractor`
  to map headers/cookies to your principal yourself.

Non-axum hosts: use
[`cleverhans-ws-core`](https://crates.io/crates/cleverhans-ws-core)'s
framework-neutral session loop directly.
