# CleverHans Protocol Specification

**Version:** 0.1.2-draft
**Status:** Draft
**Tracking:** ED-536

*Changes in 0.1.2: standalone service topology (§10.2), host webhook contract
(§14) with its own independent version integer, security invariants 11–14
(§12). All additive; the wire version remains `0.1`.*

*Changes in 0.1.1: normative streaming semantics for `ChatMessage` (§6.3),
context-summary guidance (§5), slot builders (§9.7), agent-mandate seam
(§9.8), state-as-string rationale (§11). All additive; the wire version
remains `0.1`.*

A propose-only, in-app, human-in-the-loop (HITL) agent protocol. This document is the
foundational artifact of the framework: it defines the **proposal envelope**, the
**proposal lifecycle**, and the **seam interfaces** an application implements to host an
agent. It is language- and transport-agnostic; reference implementations (Rust backend,
TypeScript frontend, gRPC binding) conform to this spec but do not define it.

---

## 1. Core thesis

> The agent **never acts on the system**. It proposes actions (and dynamic UI) against an
> application that already knows its own state; the application executes through its own
> normal authorized path. The user is the executor.

Every rule in this spec derives from that sentence. Two orthogonal safety gates hold at
all times:

1. **Closed vocabulary** — the agent can only *reference* actions and UI block types that
   the application has registered. It has no generative surface into either.
2. **Naming is not firing** — referencing an action produces a *proposal*. Nothing
   executes until the user confirms, and execution runs under the user's own credentials
   through the application's own authorization path.

The agent holds no write credentials. Its only secret is an LLM provider key. It cannot
escalate beyond what the user could do by hand, because it never does anything by hand.

## 2. Roles and terminology

| Term | Meaning |
|---|---|
| **App** | The host application: its frontend, backend, data, and authorization model. |
| **Agent** | The framework component that talks to an LLM and emits proposals. Runs in-process with the app backend in the reference topology (§10). |
| **User / Principal** | The authenticated human. All authorization and execution happen as this principal. |
| **Action** | A named, registered operation the app can perform. Identified by an inert action ID. |
| **Action registry** | The closed enumeration of actions; the shared contract between app and agent (§4). |
| **Proposal** | An envelope message in which the agent references one action with filled parameters, for the user to confirm or reject. |
| **Block type** | A registered UI component identifier. The agent selects a block type and fills its slots; it never generates markup (§8). |
| **Envelope** | The transport-agnostic message set defined in §6. The envelope is the stable artifact; actions and blocks evolve independently of it. |
| **Seam** | An interface the app implements to plug into the framework (§9). |

Requirement keywords **MUST**, **MUST NOT**, **SHOULD**, **MAY** follow RFC 2119.

## 3. Action IDs

- An action ID is an **inert, hand-authored, opaque key** — e.g.
  `transaction.coBuyer.remove`. Dotted-path naming is a readability convention only.
- The ID is a **key, not a description**. It carries no route, no function path, no
  encoded semantics. This is what keeps frontend and backend decoupled: each side keeps
  its own private mapping off the ID, and that mapping never appears in the contract.
  - Frontend (private): `id → route / component`
  - Backend (private): `id → handler + real authorization + real queries`
- The action set is a **closed enumeration owned by the app**. The agent MAY reference a
  registered ID; it MUST NOT synthesize one. Any proposal naming an unregistered ID is
  invalid at validation (§7) and MUST NOT be rendered.
- Learned or information-bearing ID schemes (e.g. RQ-VAE-style semantic codes) are
  **explicitly rejected** for the action path: a synthesizable ID is a security liability
  in compliance settings. Embedding/vector techniques MAY be used only as a *retrieval
  pre-filter* over registry descriptions (top-k shortlisting for large registries, §5);
  they never resolve an action.

## 4. Action registry — the shared contract

The registry is the single source of truth both sides reference; neither side owns the
other. In the reference implementation it is defined once in Rust (where authorization
and schemas live) and TypeScript types are generated from it — one source, three
consumers: backend resolver, frontend router, agent tool list.

Each registry entry carries:

| Field | Type | Purpose |
|---|---|---|
| `id` | string | Inert key (§3). The seam. |
| `description` | string | What the model matches user intent against. **Load-bearing**: this is the tuning and evaluation surface, especially for weaker/local models. |
| `params` | param schema | Typed parameter schema. Every param is tagged with a `source` (§4.1). |
| `block_type` | string | Which registered UI block renders proposals for this action (§8). |
| `mutates` | bool | If true, the action MUST go through dry-run (§7.2) and explicit confirmation. |
| `authz` | authz reference | Permission requirement, checked against the principal via the authz seam (§9.3). |
| `handler` | handler reference | Backend-side executor (§9.2). Not part of the wire contract. |
| `dry_run` | handler reference, optional | Backend-side preview computation (§9.2). REQUIRED when `mutates` is true. |

`handler` and `dry_run` are registry-resident but backend-private: they never cross the
envelope.

*Registry serialization (non-normative).* The wire-visible registry data — every field
above except `handler` and `dry_run` — MAY be authored as a versioned declarative
document. The reference implementation uses a JSON file with a `spec_version` field
(§13): the framework loads it, the app attaches handlers by action ID at startup, and
the build-time invariants above are enforced after attachment. The same document is the
codegen input (§9) and the interchange format for conformance fixtures and non-Rust
registry authors. It never crosses the wire.

### 4.1 Parameter sources

Every parameter is tagged `source: context` or `source: utterance`:

- **`context`** — derived from application state (e.g. `transactionId` from the current
  route). Filled by the framework from the app-provided context snapshot (§6.2). The
  model MUST NOT be able to set, override, or observe-and-echo these into the filled
  value; the framework fills them after model output, from context alone.
- **`utterance`** — derived from what the user said (e.g. `country = 'NO'`). Emitted by
  the model and validated against the param schema before the proposal is accepted.

### 4.2 Record identity

The model emits **intent or a predicate**; the **app resolves concrete record
identity**. The model MUST NOT emit lists of record IDs. For bulk operations the action
takes a predicate parameter (e.g. `deleteByPredicate(...)`) and the app evaluates the
predicate under the user's own data-access rules (RLS or equivalent) to find matches —
surfaced to the user via dry-run preview (§7.2).

## 5. Intent resolution

How the agent maps a user request onto the closed action set:

1. **Select.** Registry entries are presented to the model as tool definitions; the model
   emits a structured tool call. Small registry → present all entries in context. Large
   registry → embed `description` fields, retrieve top-k candidates, present the
   shortlist; the model still performs the selection. Retrieval never resolves.
2. **Fill.** Utterance-sourced params come from the model's tool call. Context-sourced
   params are filled by the framework from the current context snapshot (§4.1).
3. **Validate.** Full propose-time validation (§7.1) before anything is rendered.
4. **Low confidence → ask, never force-fit.** If selection is ambiguous, the agent
   SHOULD disambiguate (chat question or a disambiguation block) or decline. A wrong
   proposal is safe (the user won't confirm it) but erodes trust; declining is preferred
   over guessing.

**Context summaries.** An implementation MAY surface an app-controlled summary of the
current context (route, view kind, whether a selection exists) to the model to aid
selection. The summary is informational only: it never substitutes for framework-side
param filling (§4.1), and it SHOULD omit record identifiers — the model resolves
intent, the app resolves identity (§4.2), and an ID the model never saw is an ID it
cannot echo.

## 6. The envelope

The envelope is the transport-agnostic message set exchanged between the app frontend
and the agent over a bidirectional stream. **The envelope defines message shapes only —
never actions.** `action_id` is a plain string; `params` is a generic typed map. This is
what lets the envelope stay stable while the registry evolves freely.

A conforming binding (gRPC, WebSocket + JSON-RPC, tRPC, …) MUST carry every message and
field below with equivalent semantics. The reference binding is a gRPC bidirectional
stream (§11).

### 6.1 Session

A session is one authenticated stream between one frontend and the agent, bound to one
principal. The first client message MUST be `Init`. The agent MUST reject envelope
traffic on unauthenticated streams (§10).

### 6.2 Client → agent: `ClientEvent`

| Message | Fields | Semantics |
|---|---|---|
| `Init` | `spec_version`, `context` | Opens the session. `context` is an initial context snapshot. |
| `ContextUpdate` | `context`, `context_seq` | Replaces the current context snapshot. `context_seq` is a client-monotonic sequence number; proposals record the snapshot they were built against (§6.4). |
| `UserMessage` | `text`, `client_msg_id` | A user chat turn. |
| `ConfirmAction` | `proposal_id` | The user confirms a proposal. Triggers confirm-time revalidation and execution (§7.3). |
| `RejectAction` | `proposal_id`, `reason?` | The user declines. Terminal for that proposal; the reason MAY be fed back to the model as conversational context. |

**Context shape is an app seam** (§9.5): the envelope treats `context` as a structured,
app-defined value. The reference shape covers the common case:

```
context = {
  route:               string      // current app route/location
  params:              map         // route or view parameters
  selected_record_id:  string?     // current selection, if any
  view_type:           string?     // e.g. "detail" | "list" | ...
  extensions:          map         // app-defined additions
}
```

Context flows one way: app → agent. It is the *only* channel through which
context-sourced params get filled, which is why the model never touches them.

### 6.3 Agent → client: `ServerEvent`

| Message | Fields | Semantics |
|---|---|---|
| `ChatMessage` | `msg_id`, `text`, `done` | Assistant prose; see the streaming contract below. |
| `ActionProposal` | see §6.4 | A validated proposal, ready to render. |
| `ProposalStateChanged` | `proposal_id`, `state`, `reason?`, `result?` | Lifecycle transitions after emission (§7): `executed` (with `result`), `failed`, `expired`, `rejected` (echo). |
| `Error` | `code`, `message`, `recoverable` | Stream- or turn-level errors that are not proposal state changes. |

**Streaming contract for `ChatMessage`.** One text segment MAY be delivered as any
number of `done: false` messages followed by exactly one `done: true` message, all
sharing one `msg_id`:

- `done: false` messages carry incremental *fragments* in order.
- The closing `done: true` message carries the **authoritative full text** of the
  segment — not a fragment. A client that ignores every `done: false` message and
  renders only `done: true` messages MUST end up with a complete, correct transcript.
- Clients MUST key accumulation by `msg_id` and MUST replace any accumulated fragments
  with the `done: true` text.
- A non-streaming sender emits a single `done: true` message per segment.

### 6.4 The proposal message

```
ActionProposal = {
  proposal_id:   string        // agent-generated, opaque, unique per session
  action_id:     string        // MUST be a registered action ID
  params:        map           // fully filled: context-sourced + validated utterance-sourced
  block_type:    string        // MUST be a registered block type (normally the action's)
  slots:         map           // typed slot values for that block (§8)
  preview:       DryRunPreview?  // REQUIRED for mutating actions
  context_seq:   int           // the context snapshot this proposal was built against
  turn_msg_id:   string?       // correlates to the ChatMessage turn that produced it
}

DryRunPreview = {
  affected_count: int          // how many records the action would touch
  sample_ids:     string[]     // a bounded sample of affected record identifiers
  summary:        string?      // optional human-readable one-liner
  extensions:     map          // app-defined preview payload (e.g. a diff)
}
```

Invariants:

- A proposal that reaches the frontend is **already validated** (§7.1). Frontends MUST
  NOT receive — and MUST refuse to render — proposals naming unregistered actions or
  block types, or with slot values that fail the block schema.
- `preview` MUST be present when the referenced action has `mutates: true`, and MUST
  have been computed under the principal's own data-access rules, so the preview is
  permission-correct: the user sees exactly what *they* can affect, nothing more.
- Proposals are immutable once emitted. A changed intent is a **new proposal**; the old
  one expires (§7.4).

## 7. Proposal lifecycle

```
                    ┌──────────┐
       model emits  │ proposed │  (agent-internal)
                    └────┬─────┘
              validation │
            ┌────────────┼────────────┐
            ▼                         ▼
       ┌─────────┐               ┌─────────┐
       │ invalid │ (terminal,    │validated│──── emitted to frontend
       └─────────┘  never        └────┬────┘
                    rendered)         │
                     ┌────────────────┼─────────────────┐
                     ▼                ▼                  ▼
                ┌─────────┐     ┌───────────┐      ┌─────────┐
                │ expired │     │ confirmed │      │rejected │
                └─────────┘     └─────┬─────┘      └─────────┘
                                      │ confirm-time revalidation
                            ┌─────────┼─────────┐
                            ▼         ▼         ▼
                      ┌─────────┐ ┌────────┐ ┌────────┐
                      │ expired │ │executed│ │ failed │
                      └─────────┘ └────────┘ └────────┘
```

States `proposed` and `invalid` exist only agent-side; the frontend first observes a
proposal in `validated`. Terminal states: `invalid`, `expired`, `rejected`, `executed`,
`failed`.

### 7.1 Propose-time validation (`proposed → validated | invalid`)

Run by the framework before any proposal is emitted. All checks MUST pass:

1. **Existence** — `action_id` is registered; `block_type` is registered.
2. **Params typecheck** — utterance-sourced params validate against the param schema;
   context-sourced params were filled from context (never from model output); no
   unknown params.
3. **Authorization** — the authz seam (§9.3) allows this principal to perform this
   action with these params. An unauthorized action MUST NOT be proposed: the gate is at
   propose time *and* confirm time, not confirm time only.
4. **Slot typecheck** — slot values validate against the block type's slot schema.
5. **Dry-run** — if `mutates: true`, the dry-run seam (§9.2) is invoked under the
   principal and its preview attached.

On failure the proposal becomes `invalid` and is never rendered; the agent SHOULD
respond conversationally instead (disambiguate, decline, or explain).

### 7.2 Dry-run

`dry_run(params, principal) → DryRunPreview` is the app's answer to "what would this
do?" It MUST be side-effect-free and MUST be computed under the principal's own
data-access rules. The framework makes no assumption about *how* (SQL, RLS, service
call) — the seam is just the function signature (§9.2).

### 7.3 Confirmation and execution (`validated → confirmed → executed | failed | expired`)

On `ConfirmAction`:

1. **Revalidate.** The full §7.1 pipeline runs again against *current* state — the world
   may have changed since the proposal was rendered (records deleted, permissions
   revoked, context moved on). Revalidation failure → `expired`, reported via
   `ProposalStateChanged`; nothing executes.
2. **Execute.** The framework invokes the registered `handler(params, principal)` —
   the app's own code, running under the user's own credentials and authorization path.
   The agent contributes nothing to this call but the validated params.
3. **Report.** Success → `executed` with `result`; handler error → `failed` with
   `reason`. Both flow back as `ProposalStateChanged` and MAY be fed to the model as
   context for the next turn.

A `ConfirmAction` for an unknown, already-terminal, or expired `proposal_id` MUST be
answered with `ProposalStateChanged` reflecting the actual state (or `Error` for
unknown IDs) — it MUST NOT execute anything.

### 7.4 Expiry (`validated → expired`)

Implementations SHOULD expire pending proposals when the context snapshot they were
built against (`context_seq`) is superseded in a way that invalidates them, and MAY
expire on TTL. Confirm-time revalidation (§7.3) is the backstop that makes expiry a UX
concern rather than a safety one: a stale confirm can never execute against a state it
wasn't validated for.

## 8. Dynamic UI — block types and slots

The agent does not generate UI. It selects a **block type** from a closed, app-registered
enum and fills its typed **slots**. Dynamic is the *combination* — which block, which
slot values; the *vocabulary* is fixed, which is what keeps rendered UI auditable.

- Block types are registered alongside actions (same registry source; same codegen).
  Each declares a slot schema.
- The frontend owns one component per block type. The framework's **block-type router**
  ("proposal arrived → mount the registered component for `block_type`, pass `slots`")
  is framework code even in a headless setup; app components are presentational and do
  not know a stream exists.
- Slot values MUST validate against the block's slot schema at propose-time (§7.1).
  Frontends MUST refuse to render unregistered block types (defense in depth; such a
  proposal should have been `invalid`).
- **Slot content SHOULD come from app code, not model output.** The reference
  implementation sources slot values from an app-registered slot builder (§9.7): the
  model contributes intent and utterance params only, so even the rendered *text* of a
  proposal is app-authored. An implementation MAY let the model author slot values, but
  they MUST still pass the slot schema before render.

The framework MAY ship an optional batteries-included block pack (confirm, diff,
bulk-preview, form) as a **separate package** — never a core dependency.

## 9. Seam interfaces

What the app implements. Signatures are given in language-neutral pseudocode; the Rust
reference lib expresses them as traits, the TS lib as interfaces.

**Framework owns:** the envelope and protocol, the propose-only state machine and
dry-run hook, the registry abstraction, the validation pipeline (propose-time and
confirm-time), the agent loop, and the frontend block router.

**App plugs in:** everything below.

### 9.1 Action definitions

Registry entries per §4 — the app authors them; the framework only reads them.

### 9.2 Handlers

```
handler(params: ValidatedParams, principal: Principal) -> Result<HandlerResult, HandlerError>
dry_run(params: ValidatedParams, principal: Principal) -> Result<DryRunPreview, HandlerError>   // required iff mutates
```

Handlers are the app's normal execution path. The framework never constructs a
principal; it threads through the one the transport authenticated (§10). No SQL, RLS,
or storage assumption — `dry_run` is just a function that returns a permission-correct
preview.

### 9.3 Authorization resolver

```
authorize(principal: Principal, action_id: string, params: ValidatedParams) -> Allow | Deny(reason)
```

Called at propose time and again at confirm time. The framework treats it as opaque; the
app maps it onto its real permission system.

### 9.4 LLM provider

```
LlmProvider:
  complete(messages, tools) -> stream of (text deltas | tool calls)
```

The single seam through which model access flows. Self-hosting story lives entirely
here: BYO API key, fully local (e.g. Ollama, zero egress), or an internal gateway.
Weaker local models put more weight on registry `description` quality and on
validation/disambiguation — propose-only makes a bad selection safe, not silent; ship
action-mapping evals (utterance + context → expected action) against recommended local
models.

### 9.5 Context shape

The app defines what a context snapshot contains (§6.2 gives the reference shape) and
how context-sourced params are extracted from it. The framework only guarantees the
one-way flow: context comes from the app, and only the framework — never the model —
writes context-sourced params.

### 9.6 Block components

One frontend component per registered block type, receiving typed slots. Registered with
the block router; otherwise plain app code.

### 9.7 Slot builders

```
build_slots(params: ValidatedParams, preview: DryRunPreview?) -> Slots
```

Backend-side, per action, optional (absent → empty slots). Turns validated params and
the dry-run preview into the slot values for the action's block type. Runs inside
propose-time validation, before the slot schema check (§7.1 step 4), so its output is
schema-validated like any other slot source. Keeping slot content app-authored closes
the last generative surface in the rendered UI (§8): the model selects, the app phrases.

### 9.8 Agent mandate (optional)

```
mandate(principal: Principal, action_id: string, params: ValidatedParams,
        preview: DryRunPreview?) -> Allow | Deny(reason)
```

A second, optional permission layer distinct from §9.3: not "may this user do X" but
"did this user authorize the *agent* to bring them X". It expresses the delegation
contract between user and agent — e.g. "never bulk deletes", "nothing touching more
than 20 records", "read-only actions only".

- A mandate MUST only **narrow**: effective permission is
  `authorize(principal, …) ∧ mandate(principal → agent, …)`. It can never grant
  anything the app's authorization denies.
- It SHOULD be evaluated at propose time so an out-of-mandate proposal is `invalid`
  and never renders — this is the defense against confirmation fatigue: a
  prompt-injected model cannot even *ask* for an action outside the mandate.
- Because mandates naturally reference blast radius, the hook receives the dry-run
  preview; implementations evaluating `affected_count` rules run the mandate after
  the dry-run step of §7.1.
- Like every seam, the policy engine behind it is the app's choice. Analyzable policy
  languages (e.g. Cedar) suit user-editable, auditable mandates well; the closed
  action registry enumerates directly into such a policy schema. This spec mandates
  the hook's semantics, not an engine.

## 10. Deployment topology and auth chain

### 10.1 In-backend (reference topology)

Reference topology: the agent runs **in-backend** — a crate/module inside the app
backend process, not a separate service. Lowest latency, shares the request's
authorization context natively, no cross-service auth surface.

Auth chain:

```
user ↔ frontend            app's normal auth (OAuth 2.1 in the reference stack)
frontend ↔ agent           the envelope stream, authenticated AS THE USER
agent ↔ LLM                LlmProvider; the provider key is the agent's only credential
agent → backend            proposals only; execution happens on confirm,
                           via app handlers, under the user's credentials
```

The agent has no standing write access to anything. The agent feature MUST be cleanly
optional (feature flag, no degraded UI when off).

### 10.2 Standalone service topology

The agent MAY run as a **separate process** — a service the app deploys next to its
backend (the reference binary is `cleverhans serve`). Everything the framework owns
stays inside the service: the envelope and state machine, the validation pipeline, the
agent loop, context-param filling, and slot building. Everything the app owns — handlers
(§9.2), dry-run (§9.2), authorization (§9.3), and transport authentication — is reached
over HTTP webhooks defined by the host webhook contract (§14).

Auth chain:

```
user ↔ frontend            app's normal auth (unchanged)
frontend ↔ service         the envelope stream; the upgrade's credentials are
                           verified against the host's verify_session endpoint (§14.3) —
                           the framework still never constructs a principal
service ↔ LLM              LlmProvider; provider key held by the service
service → host             webhook calls (§14), authenticated service-to-service;
                           they carry the principal AS DATA, never user credentials
host → anything            execution under the host's own handler and authorization
                           path, exactly as §10.1
```

The service topology moves the §9 seam boundary onto the network; it does **not** move
the trust boundary of the propose-only model. The host remains the sole execution
authority: the service can only deliver validated, confirmed proposals to endpoints the
host implements, authenticates, and executes under its own rules.

## 11. Transport bindings

The envelope (§6) is the artifact; transport is a binding detail. A binding MUST provide:
an authenticated bidirectional stream per session, ordered delivery per direction, and a
faithful encoding of every envelope message.

**Reference binding: gRPC bidirectional stream.** The proto defines the envelope only —
`action_id` as a plain string, `params`/`slots` as generic structures
(`google.protobuf.Struct`, or per-action messages if stringly-typed maps bite). The
proto never enumerates actions; the registry evolves without proto changes.

**Lifecycle state is a string on the wire, not a proto enum.** `ProposalStateChanged.state`
carries the §7 state name (`"executed"`, `"expired"`, …) as a plain string, deliberately:

- The state set is already normative in §7 — a wire enum would duplicate the source of
  truth without adding safety the validation pipeline doesn't provide.
- Proto3 decodes an unrecognized enum value to `0`/`UNSPECIFIED`, so adding a state
  would make old clients silently *misread* new states. An unknown string degrades
  readably instead, keeping envelope evolution additive (§13).
- Strings keep every binding (JSON-RPC, tRPC, …) identical on this field instead of
  each inventing its own enum mapping.

Frontends MUST treat an unrecognized state as terminal and act on it no further —
fail closed, render nothing new.

Other conforming bindings (WebSocket + JSON-RPC, tRPC, SSE + POST upstream) are
explicitly welcome. Language-neutral conformance vectors for the behavior this
spec mandates live in [`spec/vectors/`](vectors/README.md); a conforming
implementation is expected to pass them. MCP is intentionally **not** the in-app transport — it earns its
place only for *external* agents driving the same action registry; for a first-party
in-app agent it is overhead.

## 12. Security invariants (normative summary)

1. The agent MUST NOT hold write credentials to the app. Its only credential is the LLM
   provider key.
2. The agent MUST only reference registered action IDs and block types; anything else is
   `invalid` and unrendered.
3. A proposal MUST NOT execute anything. Execution requires explicit user confirmation.
4. Execution MUST run through the app's own handler and authorization path, as the
   authenticated principal.
5. Context-sourced params MUST be filled by the framework from app context, never by
   the model.
6. The model MUST NOT emit concrete record-ID lists; bulk intent is expressed as
   predicates the app resolves under the user's data-access rules.
7. Dry-run previews MUST be side-effect-free and computed under the principal's own
   permissions.
8. Confirm-time revalidation MUST run the full validation pipeline; a stale or
   tampered confirm MUST NOT execute.
9. Authorization MUST be checked at propose time and again at confirm time.
10. The rendered UI vocabulary MUST be closed: registered block types, typed slots,
    schema-validated before render.

Invariants 11–14 apply to the standalone service topology (§10.2):

11. **Webhook endpoints are execution surface.** A host MUST authenticate every webhook
    call as coming from its agent service (bearer secret at minimum; mTLS where
    available) and MUST NOT expose these endpoints to end users or the public internet
    unauthenticated.
12. **No standing user credentials in the service.** The service forwards the client's
    transport credentials to `verify_session` exactly once, at session establishment,
    and MUST NOT retain or re-forward them. After establishment, webhook calls carry
    the principal *as data*, never a credential. (Extends invariant 1: the service's
    only credentials are the LLM provider key and the host service secret.)
13. **The principal is opaque and echoed verbatim.** The service MUST send the
    `principal` value returned by `verify_session` byte-identical on every webhook call
    for that session and MUST NOT construct, merge, or mutate it. A host that places
    the service outside its trust boundary SHOULD make the principal a verifiable token
    (signed claims or an opaque session reference) and re-verify it per call — the wire
    shape is identical either way; that is the point of opacity.
14. **Execute is idempotent.** Hosts MUST implement the execute endpoint idempotently,
    keyed on the request's `idempotency_key` (§14.6). This is what makes bounded retry
    after delivery failure safe.

## 13. Versioning

- **Spec:** semver on this document; `Init.spec_version` lets endpoints negotiate or
  reject.
- **Envelope:** additive evolution only within a major version — new optional fields
  and new message types; existing field semantics never change.
- **Registry:** evolves freely and independently of the envelope; adding an action is
  an app-side edit plus codegen, with no protocol change. This decoupling is the point.
- **Webhook contract (§14):** versioned independently of this document, as an integer
  carried in `X-CleverHans-Webhook-Version`. Like the gRPC and WS bindings, the webhook
  contract is a binding-layer artifact: it evolves additively (new optional fields, new
  optional endpoints) without touching envelope or registry versioning.

## 14. Host webhook contract (service topology)

The normative wire contract between an agent service (§10.2) and the host application.
A host in any language becomes serve-compatible by implementing these endpoints;
conformance vectors live in `spec/vectors/webhook/` and machine-readable body schemas
(non-normative implementer aids) in `spec/webhook/schemas/`.

### 14.1 Model

Four HTTP POST endpoints, host-implemented, service-called. The host never calls the
service. All bodies are JSON (`application/json`). Endpoint paths are deployment
configuration.

| Endpoint | Required | Seam |
|---|---|---|
| `verify_session` | yes | transport authentication (§10.2) |
| `authorize` | yes | §9.3 — a host with no per-action permissions returns `{"decision":"allow"}` unconditionally; a trivial handler is cheaper than an optional endpoint in the contract |
| `dry_run` | iff any registered action has `mutates: true` | §9.2 dry-run |
| `execute` | yes | §9.2 handler |

Slot building and context-param resolution do **not** cross the wire: context params are
filled by the service from the registry's `context_params` mapping (§4.1), and slots come
from declarative slot configuration in the service (§9.7 semantics, app-authored). A
`build_slots` webhook is a reserved future additive endpoint.

### 14.2 Headers

| Header | Semantics |
|---|---|
| `Authorization: Bearer <service-secret>` | Service-to-service credential from deployment config. The host MUST verify it on all endpoints (invariant 11). |
| `X-CleverHans-Webhook-Version: 1` | Contract version. The host MUST reject an unknown version with `400` and body `{"error": "unsupported_webhook_version", "supported": [1]}`; the service MUST treat that as fatal misconfiguration — fail closed, log, do not degrade. |
| `X-CleverHans-Delivery: <uuid>` | Unique per HTTP attempt (changes on retry). Log correlation only; NOT the idempotency key. |
| `Content-Type: application/json` | Both directions. |

The user's credentials appear only in the `verify_session` body, never in the headers of
the other calls, and never after session establishment (invariant 12). Payload HMAC
signing (`X-CleverHans-Signature`) is a reserved future additive header.

### 14.3 `verify_session`

Called once per envelope-stream establishment (e.g. WebSocket upgrade), before any
envelope traffic.

Request:

```
{ "webhook_version": 1,
  "session_id": "s_9f2…",
  "headers": { "authorization": "Bearer eyJ…", "cookie": "sid=…" } }
```

`headers` contains only the configured forward-allowlist (default: `authorization`,
`cookie`), keys lowercased.

| Host response | Service behavior |
|---|---|
| `200 {"principal": <any JSON>}` | Stream established. `principal` is stored verbatim for the session and echoed on every subsequent call (invariant 13). Whether it is plain claims or a signed/reference token the host re-verifies per call is the host's choice. |
| `401` / `403` (optional body `{"reason": "…"}`) | Stream refused with the same status. |
| Any other status, malformed body, timeout, network error | Stream refused with `503`. Fail closed. |

### 14.4 `authorize`

Request:

```
{ "webhook_version": 1,
  "kind": "authorize",
  "session_id": "s_9f2…",
  "action_id": "transaction.coBuyer.remove",
  "params": { "transactionId": "tx_581" },
  "principal": <verbatim echo> }
```

`params` are the fully validated, context-filled params — exactly what §9.3 receives
in-process. Called at propose time and again at confirm time (§12.9); the request does
not distinguish the phases in v1 (a `phase` field is reserved additive).

Response: `200 {"decision": "allow"}` or `200 {"decision": "deny", "reason": "…"}`.
Deny is a *successful* delivery, hence `200` — HTTP status is reserved for
transport/auth/protocol failure.

| Failure | At propose time | At confirm time |
|---|---|---|
| Non-200, timeout, network error, malformed body | Treated as deny: candidate `invalid`, unrendered | Proposal `expired` |

Fail closed, always.

### 14.5 `dry_run`

Request: same common shape with `"kind": "dry_run"`. Called during propose-time
validation and again during confirm-time revalidation (§7.3 step 1).

Response: `200 {"outcome": "preview", "preview": <DryRunPreview §6.4>}` (an empty
object is a valid preview; all fields default) or
`200 {"outcome": "rejected", "reason": "…"}`.

| Host behavior | At propose time | At confirm time |
|---|---|---|
| `{"outcome": "rejected"}` | Proposal `invalid` (unrendered; reason available to the model) | `expired` |
| 5xx / timeout / network error / malformed body | `invalid` | `expired` |
| 401/403/404/400-version (service misconfiguration) | `invalid`; service logs at error level | `expired` |

Cross-reference §12.7: no permission-correct preview → no rendered mutating proposal,
and never execution.

### 14.6 `execute`

Fires only from the confirm path, after confirm-time revalidation.

Request: common shape with `"kind": "execute"`, plus:

- `"idempotency_key"`: a UUID minted once per confirmed execution, **stable across
  retry attempts** of that execution.
- `"attempt"`: integer, 1-based, increments per retry.

| Host response | Proposal state | `ProposalStateChanged` |
|---|---|---|
| `200 {"outcome": "executed", "result": <json>}` | `executed` | `result` carries the JSON |
| `200 {"outcome": "rejected", "reason": "…"}` | `failed` | `reason` = host reason |
| 5xx / malformed 200 body | `failed` — an answered call is never retried | generic reason |
| Timeout / connection error | service MAY retry up to a bounded attempt count with backoff — safe only because of invariant 14 — then `failed` with reason `"execution outcome unknown"` | see caveat |
| 401/403/404/400-version | `failed`; fatal-misconfiguration log | generic reason |

**A `failed` state resulting from delivery failure does NOT assert non-execution; it
asserts the outcome is unknown to the agent.** The host owns the source of truth for
whether the mutation happened; the idempotency requirement (invariant 14) is what lets
a retry resolve the ambiguity rather than double-execute. Re-delivery of the same
`idempotency_key` MUST return the outcome of the first execution (or perform it if it
never happened), never execute twice.

### 14.7 Timeouts

Defaults, deployment-configurable: `verify_session` 5 s, `authorize` 5 s, `dry_run`
10 s, `execute` 30 s. Every timeout maps per the tables above; nothing times out into
an open-failed state, and only `execute` is ever retried.

### 14.8 Transport security

Service-to-host traffic runs over loopback or TLS. The reference implementation refuses
to start with a non-loopback plaintext upstream URL or without a service secret, absent
explicit `danger_`-prefixed configuration overrides. mTLS is recommended where the
deployment supports it (deployment note, not contract).

## Appendix A — worked example

User is on `/transactions/tx_581` (context: `route`, `selected_record_id = "tx_581"`)
and types *"remove the co-buyer from this transaction"*.

1. `ContextUpdate` already gave the agent the snapshot (`context_seq = 7`).
2. Model selects `transaction.coBuyer.remove` from the tool list and emits utterance
   params: `{}` (nothing needed from the utterance beyond intent).
3. Framework fills context param `transactionId = "tx_581"` — model never saw or set it.
4. Validation: action exists ✓, params typecheck ✓, `authorize(user,
   "transaction.coBuyer.remove", …)` ✓, action `mutates` → dry-run runs under the
   user's permissions → `{ affected_count: 1, sample_ids: ["cb_112"], summary:
   "Remove co-buyer Jane Doe from TX-581" }`.
5. `ActionProposal` emitted: `block_type = "confirm"`, slots
   `{ title: "Remove co-buyer", detail: "Jane Doe · TX-581" }`, preview attached,
   `context_seq = 7`.
6. Frontend block router mounts the app's `confirm` component. User clicks confirm →
   `ConfirmAction { proposal_id }`.
7. Framework revalidates against current state, then invokes the registered handler as
   the user. `ProposalStateChanged { state: "executed", result: … }` flows back; the
   model gets the outcome as context for its next turn.

At no point did the agent hold a credential, touch a record ID it invented, produce
markup, or execute anything.
