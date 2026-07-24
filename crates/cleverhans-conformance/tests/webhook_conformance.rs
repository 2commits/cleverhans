//! Webhook-topology conformance:
//!
//! 1. every agent-layer vector in `cases/` rerun with the seams replaced by
//!    their §14 webhook implementations (behavior preservation),
//! 2. every `webhook/service/` vector against the in-process service,
//! 3. every `webhook/host/` vector replayed against the known-good
//!    [`MockHost`] — proving the reference host passes its own checks.

use std::collections::BTreeMap;
use std::path::PathBuf;

use cleverhans_conformance::fixture::{AuthzScript, Layer};
use cleverhans_conformance::mock_host::HostScript;
use cleverhans_conformance::{
    Fixture, HostCheckOutcome, HostCheckTarget, HostVector, MockHost, ServiceVector, Vector,
    run_agent_vector_via_webhooks, run_host_vector, run_service_vector,
};

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

fn fixtures() -> BTreeMap<String, Fixture> {
    load_json_files::<Fixture>("fixtures")
        .into_iter()
        .map(|(_, fixture)| (fixture.name.clone(), fixture))
        .collect()
}

fn report(failures: usize, total: usize, lines: &[String]) {
    assert!(
        failures == 0,
        "{failures}/{total} vectors failed:\n{}",
        lines.join("\n")
    );
}

#[tokio::test]
async fn every_agent_case_passes_through_the_webhook_seams() {
    let fixtures = fixtures();
    let vectors = load_json_files::<Vector>("cases");
    let mut lines = Vec::new();
    let mut failures = 0usize;
    let mut ran = 0usize;
    for (file, vector) in &vectors {
        // Binding-layer vectors exercise the frame pump, which is
        // seam-independent; only agent-layer behavior transfers.
        if vector.layer != Layer::Agent {
            continue;
        }
        ran += 1;
        let fixture = fixtures
            .get(&vector.fixture)
            .unwrap_or_else(|| panic!("unknown fixture `{}`", vector.fixture));
        match run_agent_vector_via_webhooks(fixture, vector).await {
            Ok(()) => lines.push(format!("[PASS] {file}")),
            Err(err) => {
                failures += 1;
                lines.push(format!("[FAIL] {file}: {err}"));
            }
        }
    }
    assert!(ran > 0, "no agent-layer vectors found");
    report(failures, ran, &lines);
}

#[tokio::test]
async fn every_service_vector_passes() {
    let fixtures = fixtures();
    let vectors = load_json_files::<ServiceVector>("webhook/service");
    assert!(!vectors.is_empty(), "no webhook/service vectors found");
    let mut lines = Vec::new();
    let mut failures = 0usize;
    for (file, vector) in &vectors {
        assert_eq!(
            &vector.name, file,
            "vector name must match file stem: {file}"
        );
        assert_eq!(vector.layer, "webhook-service", "{file}: wrong layer");
        let fixture = fixtures
            .get(&vector.fixture)
            .unwrap_or_else(|| panic!("unknown fixture `{}`", vector.fixture));
        match run_service_vector(fixture, vector).await {
            Ok(()) => lines.push(format!("[PASS] {file}")),
            Err(err) => {
                failures += 1;
                lines.push(format!("[FAIL] {file}: {err}"));
            }
        }
    }
    report(failures, vectors.len(), &lines);
}

#[tokio::test]
async fn the_mock_host_passes_every_host_vector() {
    const SECRET: &str = "host-check-secret";
    let fixtures = fixtures();
    let vectors = load_json_files::<HostVector>("webhook/host");
    assert!(!vectors.is_empty(), "no webhook/host vectors found");
    let mut lines = Vec::new();
    let mut failures = 0usize;
    for (file, vector) in &vectors {
        assert_eq!(
            &vector.name, file,
            "vector name must match file stem: {file}"
        );
        assert_eq!(vector.layer, "webhook-host", "{file}: wrong layer");
        let fixture = fixtures
            .get(&vector.fixture)
            .unwrap_or_else(|| panic!("unknown fixture `{}`", vector.fixture));
        let host = MockHost::spawn(
            fixture.clone(),
            AuthzScript::default(),
            HostScript::new(),
            SECRET,
        )
        .await;
        let target = HostCheckTarget::new(host.base_url(), SECRET);
        match run_host_vector(&target, vector).await {
            // MockHost implements every endpoint incl. optional ones, so a
            // SKIP here would be a regression.
            Ok(HostCheckOutcome::Passed) => lines.push(format!("[PASS] {file}")),
            Ok(HostCheckOutcome::Skipped(reason)) => {
                failures += 1;
                lines.push(format!("[FAIL] {file}: unexpectedly skipped: {reason}"));
            }
            Err(err) => {
                failures += 1;
                lines.push(format!("[FAIL] {file}: {err}"));
            }
        }
    }
    report(failures, vectors.len(), &lines);
}
