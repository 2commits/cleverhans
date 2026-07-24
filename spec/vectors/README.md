# Conformance vectors

Language-neutral test vectors for the CleverHans protocol (`spec/SPEC.md`).
Each vector scripts every nondeterministic seam — LLM output, authorization,
handlers, dry-run previews, slot content — as pure data, feeds client
envelope events in, and asserts the outbound server events. An
implementation that passes every vector exhibits the normative behavior the
referenced spec sections require.

Runners in this repo: `crates/cleverhans-conformance` (agent + binding
layers, Rust reference implementation) and
`typescript/cleverhans-react/test/conformance.test.ts` (client layer).
Third-party implementations and FFI bindings are expected to run the same
files.

## Layout

```
fixtures/          registry fixtures: a spec §4 registry document + seam scripts
cases/             agent- and binding-layer vectors (server-side behavior)
client/            client-layer vectors (frontend session semantics)
webhook/service/   webhook-service vectors: a §10.2 agent service against a scripted host
webhook/host/      webhook-host vectors: request/response pairs a §14 host must satisfy
```

A vector's `name` must equal its file stem.

## Fixtures

```jsonc
{
  "name": "co-buyer",
  "registry": { /* exactly the spec §4 declarative registry document */ },
  "scripts": {
    "<action_id>": {
      "handler":  {"return": <json>} | {"fail": "<msg>"},
      "dry_run":  {"preview": <DryRunPreview>} | {"fail": "<msg>"}
                  | {"sequence": [<behavior>...], "then": <behavior>},
      "slots":    { "<slot>": {"const": <json>} | {"param": "<name>"}
                             | {"preview": "summary"} }
    }
  }
}
```

Seam scripts live beside — not inside — the registry document: the document
rejects unknown fields by design. Every action must have a script entry.
`sequence` forms are indexed by call count (propose-time, confirm-time, …),
falling back to `then`.

## Agent/binding vectors (`cases/`)

Common fields: `name`, `description`, `spec` (section refs), `layer`
(`"agent"` | `"binding"`), `fixture`, `llm`, `authz`, optional `executions`,
`keep_deltas`, `ignore_types`, `bindings`.

- `llm` — one array of items per LLM invocation, in order:
  `{"text": "..."}` or `{"tool_call": {"name": "...", "arguments": {...}}}`.
  Script exhaustion is a vector-authoring error the runner must surface
  loudly, not a decline.
- `authz` — `{"default": "allow" | {"deny": "<reason>"}}` or
  `{"sequence": [...], "then": ...}` indexed by call count. The sequence form
  scripts propose-allow/confirm-deny for §7.3 revalidation vectors.
- `executions` — the exact ordered list of handler invocations
  (`{"action_id", "params"}`) the vector must cause. `[]` asserts nothing
  executed — this is how §12.3 ("a proposal MUST NOT execute anything") and
  §12.8 (stale confirm) are asserted negatively.

**Agent layer** drives envelope events through the implementation's
per-event handler. `steps` alternate:

- `{"send": <ClientEvent>}` — may contain `{"$ref": "NAME"}`, substituted
  with bound values before sending (vectors stay independent of
  implementation ID formats).
- `{"expect": [<matcher>...]}` — the server events emitted since the last
  `send`, after normalization.

**Binding layer** drives raw text frames through the JSON-frame session
loop (init-first ordering, malformed-frame handling — spec §6.1, §11).
`frames` entries are `{"json": <object>}` (runner serializes) or
`{"raw": "<verbatim>"}`. `expect` matches the flat outbound event list;
`expect_close: true` asserts the session ends with no further events.

## Webhook-service vectors (`webhook/service/`)

Test an agent service implementation of the §14 host webhook contract (the
`cleverhans serve` reference binary, or a third-party reimplementation). The
format extends the agent-layer case format:

- `service_config` — deployment config the runner applies to the service
  under test: `{"secret": "<service-secret>", "forward_headers": [..]?,
  "signing_key": "<hmac-key>"?}`. With `signing_key` set, the mock host
  REQUIRES a valid §14.2 signature on every delivery.
- `host` — per-endpoint response scripts, keyed `verify_session` /
  `authorize` / `dry_run` / `execute`. Each is an array indexed by call
  count (or `{"sequence": [...], "then": <entry>}`), entries:
  `{"respond": {"status": <int>, "body": <json>}}` or `{"timeout": true}`.
  An **unscripted endpoint defaults to fixture-derived behavior**: the
  runner's mock host serves the fixture's `scripts` block (`handler`
  `{"return": v}` → `{"outcome": "executed", "result": v}`, `{"fail": m}` →
  `{"outcome": "rejected", "reason": m}`; `dry_run` likewise), `authorize`
  defaults to allow, `verify_session` to
  `200 {"principal": {"user_id": "vector-user"}}`.
- `steps` — as the agent layer, plus stream establishment:
  `{"connect": {"headers": {...}?}}` attempts establishment with those
  client headers; `{"expect_connect": {"status": <int>}}` asserts the
  result (101 = established; otherwise the §14.3 refusal status).
- `expect_deliveries` — the exact ordered list of webhook calls the host
  received. Each entry: `{"endpoint", "headers"?, "body"?}` matched with
  the standard semantics (subset objects, directives). Header keys are
  lowercased. This is how §12.12/§12.13/§12.14 are asserted on the wire.

## Webhook-host vectors (`webhook/host/`)

Test a host implementation of the §14 endpoints — the third-party
conformance story for any backend language (replayed by
`cleverhans host-check`). The host is seeded with the semantics of the
named fixture. `requests` is an ordered list:

- `{"endpoint", "auth": "valid" | "invalid" | "none", "webhook_version"?,
   "body", "expect": {"status", "body"?}}`
- `auth` selects the `Authorization` bearer: the configured secret, a wrong
  value, or absent. `webhook_version` overrides the
  `X-CleverHans-Webhook-Version` header (default 1).
- `expect.status` is an integer or `{"$in": [..]}`; `expect.body` uses the
  standard matching semantics. `$bind`/`$ref` carry values across requests
  (e.g. the idempotent-replay vector binds the first execute response body
  and requires the replay to `$ref` it exactly).

## Client vectors (`client/`)

Drive a client session implementation over an in-memory transport. `steps`:

- `{"client": {"send_message" | "update_context" | "confirm" | "reject": ...}}`
- `{"server": <ServerEvent>}` — emitted through the transport.
- `{"assert": {...}}` — subset match against the session's visible state:
  `busy`, `pending_count`, `transcript` (`role`/`text`), `proposals`
  (`state`/`working`/`reason`/`result`), `last_error`.
- `{"assert_sent": {...}}` / `{"assert_sent_first": {...}}` — subset match
  against the last / first client event the session put on the wire.

## Matching semantics (normative for runners)

1. **Delta normalization (default on):** `chat_message` events with
   `done: false` are dropped from the actual stream before matching — spec
   §6.3 guarantees a client that ignores every delta stays correct, so
   vectors assert only `done: true` messages. `"keep_deltas": true` opts
   out.
2. `ignore_types` filters listed event types out entirely before matching.
3. After normalization, expected lists match actual lists **exactly in
   count and order** (per-direction ordering is normative, §11).
4. **Objects match by subset:** every expected key must exist and match;
   extra actual keys are allowed (additive envelope evolution, §13).
5. **Arrays match element-wise with exact length.**
6. Directives — an object with a single `$`-prefixed key:
   - `{"$bind": "NAME"}` matches any value and captures it (opaque IDs).
   - `{"$ref": "NAME"}` must equal the previously bound value; also legal
     inside `send` payloads, where it is substituted before sending.
   - `{"$exact": <value>}` requires exact deep equality (closes subset
     matching where an invariant demands it, e.g. §7.1's "no unknown
     params").
   - `{"$keys": [..]}` asserts an object's exact key set without pinning
     values. Also legal *beside* sibling field matchers inside a subset
     object, pinning the key set while the siblings match values.
   - `{"$absent": true}` requires the field to be missing or `null`.
   - `{"$differs": "NAME"}` matches any value that is NOT equal to the
     previously bound value (webhook layers: retry delivery IDs).
   - `{"$in": [..]}` matches any listed value (webhook-host layer: status
     sets like `[401, 403]`).
7. **Never assert implementation-authored prose** (decline messages,
   summaries) unless the text came verbatim from the vector's own `llm`
   script — matchers for conversational declines assert only
   `{"type": "chat_message", "done": true}`.

## Per-binding directives

`"bindings": {"grpc": "skip"}` marks vectors a binding adapter cannot run
faithfully (e.g. raw-frame vectors, or params relying on integers above
2^53, which the protobuf `Struct` encoding cannot carry losslessly).
Unlisted bindings run the vector.
