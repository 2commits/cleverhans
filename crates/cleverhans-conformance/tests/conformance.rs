//! Runs every agent/binding vector in `spec/vectors/cases/` against the
//! reference implementation. One failing vector must not hide the others.

use std::collections::BTreeMap;
use std::path::PathBuf;

use cleverhans_conformance::{Fixture, Vector, run_vector};

fn vectors_dir(sub: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("../../spec/vectors/{sub}"))
}

fn load_json_files<T: serde::de::DeserializeOwned>(sub: &str) -> Vec<(String, T)> {
    let dir = vectors_dir(sub);
    let mut entries: Vec<_> = std::fs::read_dir(&dir)
        .unwrap_or_else(|err| panic!("read {}: {err}", dir.display()))
        .map(|entry| entry.expect("dir entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    entries.sort();
    entries
        .into_iter()
        .map(|path| {
            let name = path
                .file_stem()
                .expect("file stem")
                .to_string_lossy()
                .into_owned();
            let json = std::fs::read_to_string(&path)
                .unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
            let value = serde_json::from_str(&json)
                .unwrap_or_else(|err| panic!("parse {}: {err}", path.display()));
            (name, value)
        })
        .collect()
}

#[tokio::test]
async fn every_vector_passes() {
    let fixtures: BTreeMap<String, Fixture> = load_json_files::<Fixture>("fixtures")
        .into_iter()
        .map(|(_, fixture)| (fixture.name.clone(), fixture))
        .collect();
    let vectors = load_json_files::<Vector>("cases");
    assert!(!vectors.is_empty(), "no vectors found");

    let mut report = Vec::new();
    let mut failures = 0usize;
    for (file, vector) in &vectors {
        assert_eq!(
            &vector.name, file,
            "vector name must match its file stem: {file}"
        );
        let fixture = fixtures.get(&vector.fixture).unwrap_or_else(|| {
            panic!(
                "vector `{}` names unknown fixture `{}`",
                file, vector.fixture
            )
        });
        match run_vector(fixture, vector).await {
            Ok(()) => report.push(format!("[PASS] {file}")),
            Err(err) => {
                failures += 1;
                report.push(format!("[FAIL] {file}: {err}"));
            }
        }
    }
    assert!(
        failures == 0,
        "{failures}/{} vectors failed:\n{}",
        vectors.len(),
        report.join("\n")
    );
}
