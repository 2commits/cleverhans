# cleverhans-evals

Action-mapping eval harness for
[CleverHans](https://github.com/2commits/cleverhans) registries: cases pair an
utterance + context with the expected action (or expected decline), and the
suite runs them through the real agent loop against your provider.

Usually consumed through the
[`cleverhans`](https://crates.io/crates/cleverhans) facade's `evals` feature:
`load_cases(json)` + `run_suite(&agent, &principal, cases)`. See
`crates/cleverhans-demo/eval-cases.json` in the repo for the case format.
