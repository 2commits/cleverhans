# Reference CleverHans host (Node, dependency-free)

A complete spec §14 host in one file — the template for integrating
CleverHans from any language via [`cleverhans serve`](../../hosting-webhooks.md).
Four endpoints, bearer + version discipline, optional HMAC signature
verification (§14.2), and the one non-negotiable: **execute idempotent on
`idempotency_key`** (§12.14).

```sh
CLEVERHANS_SECRET=s3cret CLEVERHANS_SIGNING_KEY=k node host.js

cleverhans host-check --base-url http://127.0.0.1:3000 --secret s3cret --signing-key k
# → 7/7 PASS, host is §14-conformant
```

CI runs exactly that against every commit (`host-example` job in
`.github/workflows/ci.yml`) — proving the contract stays implementable
from the spec alone, with no shared code between this host and the
framework.

Porting to your stack: keep the shape, swap the internals —

- the auth middleware → your framework's middleware
- `stateFor`/`executed` maps → your database (store the idempotency key in
  the same transaction as the mutation)
- the `dry_run`/`execute` switches → thin delegations to your existing
  domain functions (an interface tier, not duplicate endpoints)
- `signatureValid` → verify against the **raw** request bytes, before JSON
  parsing; known-answer vector in spec §14.2
