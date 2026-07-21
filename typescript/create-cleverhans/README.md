# create-cleverhans

Scaffold [CleverHans](https://github.com/2commits/cleverhans) — a propose-only,
in-app, human-in-the-loop agent framework — into an existing project:

```sh
npm create cleverhans -- --host node              # or rust | python
npm create cleverhans -- --host rust --react      # + frontend wiring
npm create cleverhans -- --host node --dir agent  # custom directory
```

Emits a `cleverhans/` directory with a starter registry document (the closed
action contract), a host stub for your stack (axum / Node WS / FastAPI), eval
cases, and a README with the exact next steps. Never overwrites existing
files, so re-running after edits is safe.

Guides: <https://github.com/2commits/cleverhans/tree/main/docs>
