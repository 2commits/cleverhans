//! End-to-end export: telemetry::init against a live OTLP http stub —
//! a telemetry event becomes an OTLP POST to /v1/metrics. Own test binary
//! because the metrics layer is installed as the process-global subscriber.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::Router;
use axum::extract::State;
use axum::routing::post;

use cleverhans_serve::telemetry::{self, TelemetrySection};

#[tokio::test(flavor = "multi_thread")]
async fn events_export_over_otlp_http() {
    let exports = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&exports);
    let app = Router::new()
        .route(
            "/v1/metrics",
            post(|State(counter): State<Arc<AtomicUsize>>| async move {
                counter.fetch_add(1, Ordering::SeqCst);
                // Empty ExportMetricsServiceResponse is valid protobuf.
                (
                    [("content-type", "application/x-protobuf")],
                    Vec::<u8>::new(),
                )
            }),
        )
        .with_state(counter);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve stub");
    });

    let section = TelemetrySection {
        otlp_endpoint: Some(format!("http://{addr}")),
        service_name: Some("cleverhans-test".to_owned()),
        export_interval_ms: Some(100),
    };
    let (guard, layer) = telemetry::init(&section)
        .expect("init")
        .expect("endpoint configured");
    let subscriber =
        tracing_subscriber::layer::SubscriberExt::with(tracing_subscriber::registry(), layer);
    tracing::subscriber::set_global_default(subscriber).expect("global subscriber");

    tracing::info!(
        target: "cleverhans::telemetry::proposal",
        action_id = "document.rename",
        state = "executed",
        "proposal state"
    );

    // Drop flushes and shuts down the provider — the export must land.
    drop(guard);
    for _ in 0..50 {
        if exports.load(Ordering::SeqCst) > 0 {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("no OTLP export reached the stub");
}
