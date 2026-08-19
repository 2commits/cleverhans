//! The metrics layer converts `cleverhans::telemetry::*` tracing events
//! into OTEL instruments — asserted against an in-memory exporter.

use opentelemetry_sdk::metrics::data::AggregatedMetrics;
use opentelemetry_sdk::metrics::{InMemoryMetricExporter, PeriodicReader, SdkMeterProvider};

use cleverhans_serve::telemetry::layer_for_provider;

fn provider_with_memory() -> (SdkMeterProvider, InMemoryMetricExporter) {
    let exporter = InMemoryMetricExporter::default();
    let reader = PeriodicReader::builder(exporter.clone()).build();
    let provider = SdkMeterProvider::builder().with_reader(reader).build();
    (provider, exporter)
}

/// Sum of all data points of a metric, panicking on non-sum aggregations.
fn metric_sum(exporter: &InMemoryMetricExporter, name: &str) -> Option<u64> {
    for resource_metrics in exporter.get_finished_metrics().expect("finished metrics") {
        for scope in resource_metrics.scope_metrics() {
            for metric in scope.metrics() {
                if metric.name() == name
                    && let AggregatedMetrics::U64(
                        opentelemetry_sdk::metrics::data::MetricData::Sum(sum),
                    ) = metric.data()
                {
                    return Some(sum.data_points().map(|point| point.value()).sum::<u64>());
                }
            }
        }
    }
    None
}

#[tokio::test(flavor = "multi_thread")]
async fn telemetry_events_become_metrics() {
    let (provider, exporter) = provider_with_memory();
    let layer = layer_for_provider(&provider);
    let subscriber =
        tracing_subscriber::layer::SubscriberExt::with(tracing_subscriber::registry(), layer);

    tracing::subscriber::with_default(subscriber, || {
        tracing::info!(
            target: "cleverhans::telemetry::proposal",
            action_id = "document.rename",
            state = "executed",
            "proposal state"
        );
        tracing::info!(
            target: "cleverhans::telemetry::delivery",
            endpoint = "execute",
            outcome = "ok",
            status = 200_u64,
            duration_ms = 12_u64,
            "webhook delivery"
        );
        tracing::info!(
            target: "cleverhans::telemetry::delivery_retry",
            action_id = "document.rename",
            attempt = 2_u64,
            "execute retry"
        );
        tracing::info!(
            target: "cleverhans::telemetry::session",
            phase = "opened",
            "envelope session opened"
        );
        tracing::info!(
            target: "cleverhans::telemetry::llm",
            outcome = "ok",
            duration_ms = 300_u64,
            "llm call"
        );
        // Non-telemetry events are ignored.
        tracing::info!(endpoint = "execute", "unrelated event");
    });

    provider.force_flush().expect("flush");
    assert_eq!(metric_sum(&exporter, "cleverhans.proposals"), Some(1));
    assert_eq!(
        metric_sum(&exporter, "cleverhans.webhook.deliveries"),
        Some(1),
        "unrelated event must not count"
    );
    assert_eq!(
        metric_sum(&exporter, "cleverhans.webhook.execute.retries"),
        Some(1)
    );
    assert_eq!(metric_sum(&exporter, "cleverhans.llm.requests"), Some(1));
}
