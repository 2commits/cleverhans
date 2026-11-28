# cleverhans-ws

WebSocket + JSON transport binding for the
[CleverHans](https://github.com/2commits/cleverhans) envelope, as an axum
router. Two mounts:

- `agent_router_from_extension("/agent", agent)` — your existing tower/axum
  auth middleware already inserts your user type as an `Extension`; zero
  extra glue.
- `agent_router("/agent", agent, extractor)` — implement `PrincipalExtractor`
  to map headers/cookies to your principal yourself.

Every router selects the subprotocol `cleverhans.v0.1` (`SUBPROTOCOL`) when
the client offers it; other offered values (for example a token) stay in the
upgrade headers for your extractor (`offered_subprotocols`). When a
non-recoverable error ends the session (spec §6.3) the router closes the
socket with code 1008 and the error code as the reason.

Non-axum hosts: use
[`cleverhans-ws-core`](https://crates.io/crates/cleverhans-ws-core)'s
framework-neutral session loop directly.
