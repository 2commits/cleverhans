//! The embedded fixture/vector copies (required because `cargo publish`
//! cannot package files outside the crate) must stay byte-identical to
//! their `spec/` originals.

use std::path::PathBuf;

fn spec(path: &str) -> String {
    let full = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("../../spec/{path}"));
    std::fs::read_to_string(&full).unwrap_or_else(|err| panic!("read {}: {err}", full.display()))
}

fn embedded(name: &str) -> String {
    let full = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("embedded/{name}"));
    std::fs::read_to_string(&full).unwrap_or_else(|err| panic!("read {}: {err}", full.display()))
}

#[test]
fn embedded_copies_match_the_spec_originals() {
    assert_eq!(
        embedded("co-buyer.json"),
        spec("vectors/fixtures/co-buyer.json"),
        "embedded/co-buyer.json diverged — re-copy from spec/"
    );
    let host_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../spec/vectors/webhook/host");
    let mut checked = 0usize;
    for entry in std::fs::read_dir(&host_dir).expect("read host vectors") {
        let path = entry.expect("entry").path();
        if path.extension().is_none_or(|ext| ext != "json") {
            continue;
        }
        let name = path.file_name().expect("name").to_string_lossy().into_owned();
        assert_eq!(
            embedded(&name),
            std::fs::read_to_string(&path).expect("read spec vector"),
            "embedded/{name} diverged — re-copy from spec/vectors/webhook/host/"
        );
        checked += 1;
    }
    assert!(checked >= 7, "expected at least 7 host vectors, found {checked}");
}
