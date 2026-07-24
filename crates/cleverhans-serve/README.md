# cleverhans-serve

The standalone CleverHans agent service (spec §10.2): host the propose-only
HITL agent next to a backend written in **any** language. Your app
implements the four HTTP endpoints of the host webhook contract
(spec §14 — `verify_session`, `authorize`, `dry_run`, `execute`); this
binary owns everything else (envelope, validation, agent loop, LLM).

```sh
cargo install cleverhans-serve   # installs the `cleverhans` binary

CLEVERHANS_UPSTREAM_SECRET=... ANTHROPIC_API_KEY=... \
  cleverhans serve --registry registry.json --config cleverhans.toml

cleverhans host-check --base-url https://your-app --secret $SECRET
cleverhans mock-host             # known-good reference host for integration tests
```

Full walkthrough: `docs/hosting-webhooks.md` in the repository. Annotated
config: [`example.cleverhans.toml`](example.cleverhans.toml).

## License

Apache-2.0.
