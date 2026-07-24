# Hosting CleverHans from any language — the webhook contract

Your backend isn't Rust, Node, or Python? Run the agent as a standalone
service and implement **four HTTP endpoints** in whatever language your app
is written in. The service holds everything the framework owns (envelope,
validation, agent loop, LLM); your app keeps everything it owns (execution,
permissions, authentication). The normative contract is
[`spec/SPEC.md` §14](../spec/SPEC.md); this page is the practical walkthrough.

```
frontend (@cleverhans/react)
   │  WebSocket envelope            ← unchanged from the in-process topology
cleverhans serve                    ← the binary; registry.json + cleverhans.toml
   │  HTTP webhooks (§14)           ← Authorization: Bearer <service secret>
your app (Go, Java, C#, PHP, …)     ← four endpoints, your normal auth path
```

## 1. Implement the four endpoints

Think of these as a new **interface tier** over your existing services, not
new endpoints to duplicate: like a GraphQL resolver or gRPC handler beside
your REST controllers, each handler is a thin envelope delegating to the
same domain functions your current API calls. Business logic stays
single-sourced. The one genuinely new capability the tier asks of you is
`dry_run` — a permission-correct "what would this do?" your API probably
doesn't have yet, and the core of the propose-only safety model (§12.7).
Four handlers total is the floor: the request body always carries
`action_id`, so one execute route and one dry-run route dispatching
internally is fully conformant (per-action routes are optional ergonomics
via the config's `{action}` wildcard).

All POST, all JSON. Verify the `Authorization: Bearer <secret>` on every one
(spec §12.11), and reject an unknown `X-CleverHans-Webhook-Version` with
`400 {"error": "unsupported_webhook_version", "supported": [1]}`.

| Endpoint | You receive | You return |
|---|---|---|
| **verify_session** | the client's forwarded auth headers (`{"headers": {"cookie": …}}`) | `{"principal": <any JSON you like>}` or `401`/`403`. Called once per stream; the principal is echoed back to you verbatim on every later call — the agent never looks inside it. |
| **authorize** | `{action_id, params, principal}` | `{"decision": "allow"}` or `{"decision": "deny", "reason": "…"}`. Called at propose *and* confirm time. No per-action permissions? Return allow unconditionally. |
| **dry_run** | same shape, `kind: "dry_run"` | `{"outcome": "preview", "preview": {"affected_count": …, "sample_ids": […], "summary": "…"}}` or `{"outcome": "rejected", "reason": "…"}`. Must be side-effect-free and permission-correct. Only needed if you have `mutates: true` actions. |
| **execute** | same shape plus `idempotency_key`, `attempt` | `{"outcome": "executed", "result": <json>}` or `{"outcome": "rejected", "reason": "…"}`. |

**The one non-negotiable: execute is idempotent on `idempotency_key`**
(spec §12.14). A delivery timeout is retried with the *same* key; your
endpoint must return the first outcome (or perform the execution if it never
happened), never execute twice. Store the key with the outcome in the same
transaction as the mutation.

Machine-readable request/response schemas (codegen-friendly, JSON Schema
2020-12) live in [`spec/webhook/schemas/`](../spec/webhook/schemas/).

### Optional: verify payload signatures

Set `signing_key_env` in the service config and every delivery carries
`X-CleverHans-Signature: t=<unix>,v1=<hex>` — HMAC-SHA256 over
`"<t>." + raw body bytes` (spec §14.2). Verifying it buys you payload
integrity past TLS termination, a bounded replay window, and a credential
that never travels the wire. Verify against the **raw** request bytes,
before any JSON parsing:

```js
import crypto from "node:crypto";

function verifySignature(header, rawBody, key, skewSeconds = 300) {
  const parts = Object.fromEntries(header.split(",").map(p => p.split("=")));
  if (Math.abs(Date.now() / 1000 - Number(parts.t)) > skewSeconds) return false;
  const expected = crypto.createHmac("sha256", key)
    .update(`${parts.t}.`).update(rawBody).digest();
  const got = Buffer.from(parts.v1, "hex");
  return got.length === expected.length && crypto.timingSafeEqual(got, expected);
}
```

Known-answer check for your implementation: key `test-signing-key`,
t `1700000000`, body `{"kind":"execute","params":{}}` →
`v1=54043b28f3ce9c05dd923645ca289ac7cee7910b87042a03b29677cef8ffdf50`.
Hosts that require signatures: run `cleverhans host-check … --signing-key $K`
and `cleverhans mock-host --signing-key $K` behaves like you should.

## 2. Check your host

```sh
cleverhans host-check --base-url https://your-app --secret $SECRET
```

Replays the conformance vectors from `spec/vectors/webhook/host/` — auth
discipline, version rejection, idempotent replay, the §14 body shapes.
Green means serve-compatible. To test *your* integration code against a
known-good counterpart first: `cleverhans mock-host` runs the reference
host with the `co-buyer` demo fixture — or with your own registry via
`--fixture my-fixture.json` (registry + scripted seams, the
`spec/vectors/README.md` fixture format).

For a full end-to-end playground of the service itself — real stateful
backend, real model, the React click-around UI — pair it with the demo
host; the recipe is at the top of
[`crates/cleverhans-demo/serve.cleverhans.toml`](../crates/cleverhans-demo/serve.cleverhans.toml):

```sh
cargo run -p cleverhans-demo -- host                       # stateful doc store behind the §14 webhooks
CLEVERHANS_UPSTREAM_SECRET=dev-secret ANTHROPIC_API_KEY=... \
  cargo run -p cleverhans-serve --bin cleverhans -- serve \
    --registry crates/cleverhans-demo/registry.json \
    --config crates/cleverhans-demo/serve.cleverhans.toml
pnpm --filter @cleverhans/playground dev                   # connects to 8787 unchanged
```

## 3. Declare your actions and run the service

`registry.json` is the same spec §4 document every CleverHans host uses.
`cleverhans.toml` maps it onto your endpoints — a full annotated example is
[`crates/cleverhans-serve/example.cleverhans.toml`](../crates/cleverhans-serve/example.cleverhans.toml):

```toml
[upstream]
base_url = "http://localhost:3000"
secret_env = "CLEVERHANS_UPSTREAM_SECRET"

[auth]
verify = "POST /internal/cleverhans/verify-session"

[authz]
endpoint = "POST /internal/cleverhans/authorize"

[llm]
provider = "anthropic"                 # or ollama (zero egress), or scripted (CI)

[actions."*"]                          # {action} substituted per action ID
execute = "POST /internal/cleverhans/{action}/execute"
dry_run = "POST /internal/cleverhans/{action}/dry-run"

[actions."record.archive".slots]       # app-authored proposal card text (§9.7)
title = { const = "Archive record" }
summary = { preview = "summary" }
```

```sh
CLEVERHANS_UPSTREAM_SECRET=... ANTHROPIC_API_KEY=... \
  cleverhans serve --registry registry.json --config cleverhans.toml
```

The service refuses to start on a misconfiguration: a plaintext non-loopback
upstream, a missing secret, an action without an execute route, a mutating
action without a dry-run route, or an uncovered required slot — each error
names the fix.

## 4. Point the frontend at it

```tsx
const session = new AgentSession(
  createWebSocketTransport("wss://your-app/agent"),   // proxy to the service,
  { context: { route: "/records", selected_record_id: null } },
);
```

Nothing else changes — `@cleverhans/react` and `@cleverhans/ui` speak the
same envelope to the service that they speak to an in-process host.

## Security posture (spec §12.11–12.14)

- The webhook endpoints are **execution surface**: never expose them to end
  users or the public internet unauthenticated.
- The user's credentials cross the wire exactly once, to `verify_session`;
  after that the principal travels **as data**, never as a credential. A
  host that wants per-call verification makes its principal a signed token
  and re-verifies it — the wire shape is identical.
- A `failed` proposal after a delivery failure means *outcome unknown*, not
  *didn't happen*. Your idempotency key is what resolves that ambiguity
  safely.
