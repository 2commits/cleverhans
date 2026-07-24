# cleverhans-webhook

The host webhook contract (spec §14) for the CleverHans standalone service
topology: serde wire types, the delivering HTTP client (per-endpoint
timeouts, idempotent execute retry, transport-security startup refusals),
and drop-in seam implementations (`ActionHandler`, `DryRunHandler`,
`AuthzResolver`, plus the `verify_session` verifier) over a JSON principal.

`cleverhans serve` is the reference deployment; any Rust host can also use
these seams in-process to reach a remote executor.

The normative contract lives in `spec/SPEC.md` §14 with machine-readable
body schemas in `spec/webhook/schemas/` and conformance vectors in
`spec/vectors/webhook/`.

## License

Apache-2.0.
