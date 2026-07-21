# cleverhans-core

Reference backend for the [CleverHans](https://github.com/nordalf/cleverhans)
propose-only HITL agent protocol: the registry (+ declarative schema), the
validation pipeline, the propose-only state machine, the agent loop, and the
seam traits an application implements (`ActionHandler`, `DryRunHandler`,
`AuthzResolver`, `ContextParamResolver`, `SlotBuilder`, `LlmProvider`).

Most integrations depend on the
[`cleverhans`](https://crates.io/crates/cleverhans) facade instead, which
re-exports this crate plus transport bindings and providers behind features.
Depend on `cleverhans-core` directly when you bring your own transport.

The `test-util` feature ships `ScriptedLlm` — a deterministic model double
that drives the whole propose → confirm → execute pipeline offline.
