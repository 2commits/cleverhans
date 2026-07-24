# cleverhans-conformance

Language-neutral conformance vector runner for the CleverHans protocol
(`spec/SPEC.md`) and its §14 host webhook contract: fixture → scripted-seam
builders, the matching engine, the webhook `MockHost`, and the host-check
replayer that `cleverhans host-check` wraps. Vectors live in
`spec/vectors/`; every binding runs the same files.

## License

Apache-2.0.
