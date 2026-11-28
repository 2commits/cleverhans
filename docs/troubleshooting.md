# Troubleshooting

## Building from source (monorepo)

- **`protoc` not found** — `cleverhans-grpc` needs the protobuf compiler:
  `brew install protobuf` / `apt-get install protobuf-compiler`. Only the
  gRPC crate; the ws/demo path builds without it.
- **`maturin develop` fails** — it needs an activated virtualenv (or `--uv`)
  and a Rust toolchain: `cd python/cleverhans-hitl && python3.12 -m venv .venv &&
  .venv/bin/pip install maturin pytest && .venv/bin/maturin develop`.
  Installing the published wheel (`pip install cleverhans-hitl`) needs neither.
- **`pnpm -r test` compiles Rust** — the workspace builds the napi module
  from source; that's expected in the monorepo. Consumers of the published
  `@cleverhans/node` get prebuilt binaries.
- **`cargo test` skips the binding crates** — `cleverhans-py` and
  `cleverhans-node` are outside `default-members`; lint them with
  `cargo clippy -p cleverhans-py -p cleverhans-node`.

## Startup errors (they're doing their job)

- **`MissingDryRun` / `UnexpectedDryRun`** — `mutates: true` ⇔ dry-run
  registered. Fix the pairing, not the flag.
- **`MissingHandler`** — a `.bind(id, ...)` never called `.handler(...)`.
- **`UnattachedAction` / `UnknownAttachment`** — schema action ids and
  `bind`/`attach` ids must match one-to-one; use the codegen'd
  `action_ids::` constants so this is a compile error instead.
- **`UnmappedContextParam`** — a `source: "context"` param has no
  `context_params` entry and you're using `schema.context_resolver()`. Add
  the mapping, or implement `ContextParamResolver` yourself.

## Runtime behavior that looks like a bug but isn't

- **"I can't propose that action: slot `title`: missing required slot"** —
  the block's slot schema requires slots your slot builder didn't produce.
  Register a slot builder for the action (or make the slot optional).
- **Proposal invalid with no record selected** — a required context param
  resolved to nothing because the app context has no selection. That's the
  propose-only contract working; the agent declines conversationally.
- **Proposals expire on navigation** — context changed, so confirm-time
  revalidation fails. By design (spec §7): the card the user confirms must
  match this moment's app state.
- **Everything re-validates at confirm time** — authz and dry-run run twice
  (propose + confirm). Slow dry-runs are felt twice; keep them cheap.

## OpenAI-compatible servers (vLLM & co.)

- **`400 … "auto" tool choice requires --enable-auto-tool-choice and
  --tool-call-parser to be set`** — every CleverHans request carries tools,
  and vLLM serves tool calls only when started with
  `--enable-auto-tool-choice --tool-call-parser <parser>`. Pick the parser
  for your model from vLLM's
  [tool-calling page](https://docs.vllm.ai/en/latest/features/tool_calling/).
- **The model's reasoning (`<think>…</think>`, or a stray `</think>`) shows up
  in the chat** — the server has no reasoning parser, so the reasoning stays
  in `content` and looks like the answer. Start vLLM with the model's
  `--reasoning-parser` (`qwen3` for Qwen3); the reasoning then moves to a
  separate field that CleverHans ignores.
- **`returned 301`/`302`/`307`** — the provider follows no redirects, so an
  API key never travels to another host. Point `base_url` at the final URL,
  version path included (`http://host:8000/v1`).

## Transport

- **Socket closes right after connect, close code `1008`** — the server
  ended the session after a non-recoverable error (spec §6.3) and names it
  in the close reason (the `error` event just before the close says the
  same): `init_required` — the first message must be `init` (spec §6.1);
  `unsupported_spec_version` — `init.spec_version` must share the server's
  major.minor (`0.1`). After such an error the server answers nothing more
  on that connection; open a new one. A close with `1006` and no reason is
  a dropped connection, not a protocol error.
- **The WebSocket handshake fails as soon as you pass `protocols`** — a
  browser fails the connection when it offers subprotocols and the server
  selects none. `createWebSocketTransport` offers `cleverhans.v0.1` first;
  the server must select it. `cleverhans-ws` and the `ws` library's default
  do; on FastAPI accept with `subprotocol=cleverhans_agent.SUBPROTOCOL`
  (see the Python quickstart).
- **`error` `stale_context`** — a `context_update` whose `context_seq` is not
  greater than the last applied one (the `init` snapshot is 0) is refused
  and not applied (spec §6.2). `AgentSession.updateContext` counts 1, 2, …
  for you; a hand-written client must do the same, per connection.
- **A message of a new type gets no answer** — by design (spec §13): a
  receiver ignores a message type it does not know, so an older server
  stays silent instead of answering `malformed_event`.
- **FastAPI: `WARNING: No supported WebSocket library detected`** — install
  `uvicorn[standard]` (or `websockets`).
- **Next.js: `WebSocket is not defined` during SSR** — upgrade: the
  transport is inert without a `WebSocket` global and the packages ship
  `"use client"`. Construct the session in client code.
