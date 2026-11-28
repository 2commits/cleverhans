# Machine-readable schemas

JSON Schemas (draft 2020-12) for the JSON forms of the CleverHans protocol.
The prose specification, [`spec/SPEC.md`](../SPEC.md), stays normative; these
files are its machine-readable form, and the Rust reference types are checked
against them. Change a schema and its Rust type together, then regenerate
the TypeScript envelope types (see below).

| File | Describes | Status |
|---|---|---|
| [`client-event.json`](client-event.json) | `ClientEvent`, app frontend → agent (SPEC.md §6.2): `Init`, `ContextUpdate`, `UserMessage`, `ConfirmAction`, `RejectAction`, and the `Context` snapshot | The machine-readable form of the JSON encoding of the envelope (SPEC.md §6) |
| [`server-event.json`](server-event.json) | `ServerEvent`, agent → app frontend (SPEC.md §6.3, §6.4): `ChatMessage`, `ActionProposal`, `ProposalStateChanged`, `Error`, and the `DryRunPreview` | The machine-readable form of the JSON encoding of the envelope (SPEC.md §6) |
| [`registry.json`](registry.json) | The declarative registry document (SPEC.md §4): blocks, actions, params, slots, display metadata, `context_params` | Describes the reference implementation's document; SPEC.md calls this serialization non-normative |

Each file is self-contained: shared shapes sit under `$defs`, with no
reference to another file. Every message and shape has its own named
definition (`Init`, `ActionProposal`, `DryRunPreview`, `ActionDef`, …) so that
types generated from these files keep the names of the spec.

## The envelope schemas

A message is a JSON object whose `type` names the variant in snake_case; the
variant's fields sit beside `type` — for a proposal, the SPEC.md §6.4 fields
sit beside `"type": "action_proposal"`. A field the Rust types default is
optional; an optional field that Rust writes as `null` when empty is also
nullable (`reason`, `preview`, `turn_msg_id`, …).

The envelope evolves additively within a major version (SPEC.md §13), so the
envelope schemas never set `additionalProperties: false`: a receiver ignores
fields it does not know. `ProposalStateChanged.state` is a plain string, not
an enum, on purpose (SPEC.md §11): a frontend treats an unrecognized state as
terminal.

### Generated TypeScript types

The envelope types of `@cleverhans/react` are generated from
`client-event.json` and `server-event.json` into
[`typescript/cleverhans-react/src/generated/envelope.ts`](../../typescript/cleverhans-react/src/generated/envelope.ts),
which is committed. Nobody edits it by hand; regenerate it from `typescript/`:

```sh
pnpm codegen         # rewrite the file (and the registry codegen output)
pnpm codegen:check   # fail when it is stale; CI's web job runs this
```

The script is
[`typescript/scripts/codegen-envelope.mjs`](../../typescript/scripts/codegen-envelope.mjs)
(json-schema-to-typescript). The generated interfaces are closed: a schema's
openness to unknown fields is a wire rule for receivers, and an index
signature in TypeScript would hide typos. The `Error` message is exported as
`ErrorMessage`, so it does not shadow the global `Error`, and the package
names the `Context` snapshot `AppContext`.

## The registry schema

The registry document is closed: an unknown field at any level is an error,
exactly as `RegistrySchema::from_json` rejects it. The schema checks
structure, value types and the `context_params` path grammar. Some rules
stay with the loader and the registry builder, because a schema cannot state
them: the `spec_version` compatibility rule (same major.minor prefix as the
implementation), unique action and block IDs, actions that name a registered
block type, and a dry-run handler exactly for mutating actions.

### Editor support

A registry document may name its schema in an optional top-level `"$schema"`
key. Editors such as VS Code then validate and autocomplete the file. The
framework never fetches the reference; it carries the value through loading
and export unchanged.

```json
{
  "$schema": "../../spec/schemas/registry.json",
  "spec_version": "0.1",
  "blocks": [],
  "actions": []
}
```

Inside this repository, use a path relative to the document, as
[`crates/cleverhans-demo/registry.json`](../../crates/cleverhans-demo/registry.json)
does. The `$id` of each schema (`https://cleverhans.dev/spec/v0.1/<file>`)
names it; it is not yet a hosted address, so a project outside this
repository points `"$schema"` at a local copy of `registry.json` until the
schemas are published.

## How the schemas stay in step with Rust

[`crates/cleverhans-core/tests/spec_schemas.rs`](../../crates/cleverhans-core/tests/spec_schemas.rs)
reads these files and fails when a Rust wire type changes without its schema,
or a schema without its type:

- each file is valid draft 2020-12 and its `$id` ends with its file name;
- every `ClientEvent` and `ServerEvent` variant has a fully populated and a
  minimal example that validates. The examples are built through an
  exhaustive `match`, so a new variant does not compile until it has one, and
  the set of variants serde accepts must equal the set the schema lists;
- per message and per nested shape (`Context`, `DryRunPreview`, every
  registry shape): the keys of the full example equal the schema's
  `properties`, the schema's `required` keys alone deserialize while dropping
  any one of them fails, and Rust accepts `null` for a property exactly when
  the schema does;
- every registry document in the repository validates (the demo, the
  conformance fixtures, the `create-cleverhans` template), and tables of
  invalid and valid edge-case documents get the same verdict from the schema
  and from `RegistrySchema::from_json`;
- every complete envelope message in [`spec/vectors/`](../vectors/README.md)
  validates: the client events the agent-, binding- and webhook-service-layer
  vectors send, and the server events the client-layer vectors script. The
  `expect` entries are subset matchers, not messages, and are not validated.

Run it with `cargo test -p cleverhans-core --test spec_schemas`.

## Other formal definitions

- **Host webhook contract (SPEC.md §14):** request and response bodies in
  [`spec/webhook/schemas/`](../webhook/schemas/), kept in step with the Rust
  wire types by `crates/cleverhans-webhook/tests/wire_schema_sync.rs`.
- **gRPC binding (SPEC.md §11):** the envelope as protobuf in
  [`crates/cleverhans-grpc/proto/cleverhans/v1/envelope.proto`](../../crates/cleverhans-grpc/proto/cleverhans/v1/envelope.proto).
  Its proto3 JSON mapping is not the JSON encoding described here.
- **Conformance vectors:** language-neutral behavior tests in
  [`spec/vectors/`](../vectors/README.md).
