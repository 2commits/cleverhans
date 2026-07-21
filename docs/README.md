# CleverHans docs

CleverHans is a propose-only, in-app, human-in-the-loop agent framework: the
agent **never acts on the system**. It proposes actions (and dynamic UI)
against an app that already knows its own state, and the app executes through
its own normal authorized path after explicit user confirmation.

The normative protocol is [`spec/SPEC.md`](../spec/SPEC.md); everything here
is the practical integration layer over it.

## Quickstarts — host the agent from your backend

- [Rust (axum)](quickstart-rust.md)
- [Node.js](quickstart-node.md)
- [Python (FastAPI)](quickstart-python.md)

## Frontend

- [React](quickstart-react.md) — headless `@cleverhans/react` or styled
  `@cleverhans/ui`

## Guides

- [Architecture](architecture.md) — how the crates/packages relate, where the
  one Rust core sits under every binding
- [Adding an action](adding-actions.md) — the registry edit → codegen →
  handlers → UI → evals loop
- [Troubleshooting](troubleshooting.md)
- [Releasing](releasing.md) — semver tags, prerelease dist-tags, the
  `scripts/set-version.sh` bump

