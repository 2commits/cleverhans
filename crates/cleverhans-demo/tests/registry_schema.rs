//! Pins `registry.json` as the single source of truth: the built registry
//! must round-trip to exactly the committed document, and the committed
//! codegen outputs must match what the document generates.

use cleverhans_codegen::{python_module, typescript_module};
use cleverhans_demo::registry::{Store, build_registry, demo_schema};

fn repo_file(relative: &str) -> String {
    let path = format!("{}/{relative}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("read {path}: {err}"))
}

#[test]
fn registry_json_is_canonical_for_the_built_registry() {
    let built = build_registry(&Store::seeded()).schema();

    assert_eq!(
        built,
        demo_schema(),
        "registry.json and the built registry disagree — edit registry.json, \
         not the builder"
    );
}

#[test]
fn committed_typescript_module_is_fresh() {
    let schema = demo_schema();

    let generated = typescript_module(&schema.actions, &schema.blocks);

    assert_eq!(
        repo_file("../../typescript/playground/src/generated/registry.ts"),
        generated,
        "generated TS is stale — run `pnpm codegen`"
    );
}

#[test]
fn committed_python_module_is_fresh() {
    let schema = demo_schema();

    let generated = python_module(&schema.actions, &schema.blocks);

    assert_eq!(
        repo_file("../../python/generated/registry.py"),
        generated,
        "generated Python is stale — run `pnpm codegen`"
    );
}
