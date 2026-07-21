# cleverhans-grpc

Reference gRPC bidi-stream binding for the
[CleverHans](https://github.com/2commits/cleverhans) envelope. The proto is
envelope-only: frames stay opaque JSON, so the registry contract never leaks
into the transport. Building requires `protoc`.

Browser frontends use the WebSocket binding
([`cleverhans-ws`](https://crates.io/crates/cleverhans-ws)) instead — browsers
cannot open native gRPC bidirectional streams.
