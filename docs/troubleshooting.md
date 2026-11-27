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

- **Socket closes right after connect** — init-first violation (spec §6.1):
  the first frame must be `init` with a compatible `spec_version`.
- **FastAPI: `WARNING: No supported WebSocket library detected`** — install
  `uvicorn[standard]` (or `websockets`).
- **Next.js: `WebSocket is not defined` during SSR** — upgrade: the
  transport is inert without a `WebSocket` global and the packages ship
  `"use client"`. Construct the session in client code.
