# cleverhans-codegen

Registry document → typed modules for
[CleverHans](https://github.com/2commits/cleverhans): TypeScript string-literal
unions + interfaces, Python `Literal` + `TypedDict`s, Rust ID constants +
params structs (for `typed_handler`). One registry edit, every consumer
type-safe.

```sh
cleverhans-codegen --schema registry.json --ts out.ts --py out.py --rs out.rs
cleverhans-codegen --schema registry.json --ts out.ts --check   # CI freshness gate
```

Library emitters (`typescript_module`, `python_module`, `rust_module`) for
build scripts. JS and Python teams without a Rust toolchain get the same
codegen through `npx cleverhans-codegen` (`@cleverhans/node`) or
`cleverhans_agent.generate_types(...)` (PyPI `cleverhans-hitl`).
