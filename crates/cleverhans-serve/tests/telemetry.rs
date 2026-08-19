//! The metrics layer converts `cleverhans::telemetry::*` tracing events
//! into OTEL instruments — asserted against an in-memory exporter.

use std::sync::Arc;

use opentelemetry_sdk::metrics::data::{AggregatedMetrics, MetricData};
use opentelemetry_sdk::metrics::{InMemoryMetricExporter, PeriodicReader, SdkMeterProvider};
use tracing_subscriber::filter::EnvFilter;
use tracing_subscriber::layer::SubscriberExt as _;

use cleverhans_core::async_trait;
use cleverhans_core::error::LlmError;
use cleverhans_core::seams::{CompletionItem, CompletionRequest, LlmProvider};
use cleverhans_core::telemetry::SessionSpan;
use cleverhans_serve::telemetry::{self, InstrumentedLlm, layer_for_provider};

fn provider_with_memory() -> (SdkMeterProvider, InMemoryMetricExporter) {
    let exporter = InMemoryMetricExporter::default();
    let reader = PeriodicReader::builder(exporter.clone()).build();
    let provider = SdkMeterProvider::builder().with_reader(reader).build();
    (provider, exporter)
}

/// Sum of a `u64` counter's data points; `None` when the instrument
/// recorded nothing (other aggregations are skipped, not reported).
fn counter_sum(exporter: &InMemoryMetricExporter, name: &str) -> Option<u64> {
    for resource_metrics in exporter.get_finished_metrics().expect("finished metrics") {
        for scope in resource_metrics.scope_metrics() {
            for metric in scope.metrics() {
                if metric.name() == name
                    && let AggregatedMetrics::U64(MetricData::Sum(sum)) = metric.data()
                {
                    return Some(sum.data_points().map(|point| point.value()).sum::<u64>());
                }
            }
        }
    }
    None
}

/// Latest value of an `i64` up-down counter, across all data points.
fn up_down_value(exporter: &InMemoryMetricExporter, name: &str) -> Option<i64> {
    let mut latest = None;
    for resource_metrics in exporter.get_finished_metrics().expect("finished metrics") {
        for scope in resource_metrics.scope_metrics() {
            for metric in scope.metrics() {
                if metric.name() == name
                    && let AggregatedMetrics::I64(MetricData::Sum(sum)) = metric.data()
                {
                    latest = Some(sum.data_points().map(|point| point.value()).sum::<i64>());
                }
            }
        }
    }
    latest
}

/// Values of one attribute key on a `u64` counter, from the first export
/// that carries it — later batches repeat the same cumulative points.
fn counter_attrs(exporter: &InMemoryMetricExporter, name: &str, key: &str) -> Vec<String> {
    for resource_metrics in exporter.get_finished_metrics().expect("finished metrics") {
        for scope in resource_metrics.scope_metrics() {
            for metric in scope.metrics() {
                if metric.name() == name
                    && let AggregatedMetrics::U64(MetricData::Sum(sum)) = metric.data()
                {
                    return sum
                        .data_points()
                        .flat_map(|point| point.attributes())
                        .filter(|attr| attr.key.as_str() == key)
                        .map(|attr| attr.value.to_string())
                        .collect();
                }
            }
        }
    }
    Vec::new()
}

/// Runs `emit` under a subscriber holding the metrics layer, and returns the
/// exporter with everything flushed.
fn metrics_for(emit: impl FnOnce()) -> InMemoryMetricExporter {
    let (provider, exporter) = provider_with_memory();
    let subscriber = tracing_subscriber::registry().with(layer_for_provider(&provider));
    tracing::subscriber::with_default(subscriber, emit);
    provider.force_flush().expect("flush");
    exporter
}

fn emit_proposal() {
    tracing::info!(
        target: "cleverhans::telemetry::proposal",
        action_id = "document.rename",
        state = "executed",
        "proposal state"
    );
}

mod metrics_layer {
    use super::{counter_attrs, counter_sum, emit_proposal, metrics_for};

    fn emit_retry() {
        tracing::info!(
            target: "cleverhans::telemetry::delivery_retry",
            action_id = "document.rename",
            attempt = 2_u64,
            "execute retry"
        );
    }

    #[test]
    fn counts_a_proposal_transition() {
        let exporter = metrics_for(emit_proposal);
        assert_eq!(counter_sum(&exporter, "cleverhans.proposals"), Some(1));
    }

    #[test]
    fn counts_a_webhook_delivery() {
        let exporter = metrics_for(|| {
            tracing::info!(
                target: "cleverhans::telemetry::delivery",
                endpoint = "execute",
                outcome = "ok",
                status = 200_u64,
                duration_ms = 12_u64,
                "webhook delivery"
            );
        });
        assert_eq!(
            counter_sum(&exporter, "cleverhans.webhook.deliveries"),
            Some(1)
        );
    }

    #[test]
    fn counts_an_execute_retry() {
        let exporter = metrics_for(emit_retry);
        assert_eq!(
            counter_sum(&exporter, "cleverhans.webhook.execute.retries"),
            Some(1)
        );
    }

    #[test]
    fn keys_execute_retries_by_action() {
        let exporter = metrics_for(emit_retry);
        assert_eq!(
            counter_attrs(&exporter, "cleverhans.webhook.execute.retries", "action_id"),
            vec!["document.rename".to_owned()]
        );
    }

    #[test]
    fn counts_an_llm_call() {
        let exporter = metrics_for(|| {
            tracing::info!(
                target: "cleverhans::telemetry::llm",
                outcome = "ok",
                duration_ms = 300_u64,
                "llm call"
            );
        });
        assert_eq!(counter_sum(&exporter, "cleverhans.llm.requests"), Some(1));
    }

    #[test]
    fn ignores_events_outside_the_telemetry_targets() {
        let exporter = metrics_for(|| {
            tracing::info!(endpoint = "execute", "unrelated event");
        });
        assert_eq!(
            counter_sum(&exporter, "cleverhans.webhook.deliveries"),
            None
        );
    }
}

/// Instruments must not sit behind `RUST_LOG`: an operator turning logs
/// down, or scoping them to a crate, would otherwise silently zero every
/// dashboard while the service looks healthy.
mod log_filtering {
    use super::{
        EnvFilter, counter_sum, emit_proposal, layer_for_provider, provider_with_memory, telemetry,
    };

    fn proposals_under(filter: &str) -> Option<u64> {
        let (provider, exporter) = provider_with_memory();
        let subscriber =
            telemetry::subscriber(EnvFilter::new(filter), Some(layer_for_provider(&provider)));
        tracing::subscriber::with_default(subscriber, emit_proposal);
        provider.force_flush().expect("flush");
        counter_sum(&exporter, "cleverhans.proposals")
    }

    #[test]
    fn the_default_level_records_metrics() {
        assert_eq!(proposals_under("info"), Some(1));
    }

    #[test]
    fn a_quiet_log_level_still_records_metrics() {
        assert_eq!(proposals_under("warn"), Some(1));
    }

    #[test]
    fn a_crate_scoped_directive_still_records_metrics() {
        assert_eq!(proposals_under("cleverhans_serve=info"), Some(1));
    }
}

mod session_span {
    use super::{SessionSpan, metrics_for, up_down_value};

    #[test]
    fn an_open_session_raises_the_gauge() {
        let exporter = metrics_for(|| std::mem::forget(SessionSpan::open()));
        assert_eq!(
            up_down_value(&exporter, "cleverhans.sessions.active"),
            Some(1)
        );
    }

    #[test]
    fn dropping_the_span_returns_the_gauge_to_zero() {
        let exporter = metrics_for(|| drop(SessionSpan::open()));
        assert_eq!(
            up_down_value(&exporter, "cleverhans.sessions.active"),
            Some(0)
        );
    }
}

struct ScriptedLlm;

#[async_trait]
impl LlmProvider for ScriptedLlm {
    async fn complete(&self, _request: CompletionRequest) -> Result<Vec<CompletionItem>, LlmError> {
        Ok(vec![CompletionItem::Text("hello".to_owned())])
    }
}

/// One model call is one event, whether the consumer drains the stream or
/// abandons it — the agent drops the stream whenever the client disconnects
/// mid-generation, and those are the calls worth seeing.
mod instrumented_llm {
    use futures_util::StreamExt as _;

    use super::{CompletionRequest, LlmProvider as _, ScriptedHarness};

    fn request() -> CompletionRequest {
        CompletionRequest {
            messages: Vec::new(),
            tools: Vec::new(),
        }
    }

    #[tokio::test]
    async fn reports_a_stream_the_consumer_abandoned() {
        let harness = ScriptedHarness::new();
        let mut stream = harness
            .llm
            .complete_stream(request())
            .await
            .expect("stream");
        let _first = stream.next().await;
        drop(stream);
        assert_eq!(
            harness.llm_requests(),
            Some(1),
            "an abandoned call must still be counted"
        );
    }

    #[tokio::test]
    async fn reports_a_drained_stream_once_even_when_polled_past_the_end() {
        let harness = ScriptedHarness::new();
        let mut stream = harness
            .llm
            .complete_stream(request())
            .await
            .expect("stream");
        while stream.next().await.is_some() {}
        let _past_the_end = stream.next().await;
        drop(stream);
        assert_eq!(
            harness.llm_requests(),
            Some(1),
            "one call is one event, not one per poll"
        );
    }

    #[tokio::test]
    async fn reports_nothing_while_the_stream_is_still_open() {
        let harness = ScriptedHarness::new();
        let _stream = harness
            .llm
            .complete_stream(request())
            .await
            .expect("stream");
        assert_eq!(
            harness.llm_requests(),
            None,
            "the event belongs to the end of the call"
        );
    }

    #[tokio::test]
    async fn reports_a_buffered_call() {
        let harness = ScriptedHarness::new();
        let _items = harness.llm.complete(request()).await.expect("items");
        assert_eq!(harness.llm_requests(), Some(1));
    }
}

/// A subscriber-scoped provider plus the wrapped LLM under test.
struct ScriptedHarness {
    provider: SdkMeterProvider,
    exporter: InMemoryMetricExporter,
    llm: InstrumentedLlm,
    _guard: tracing::subscriber::DefaultGuard,
}

impl ScriptedHarness {
    fn new() -> Self {
        let (provider, exporter) = provider_with_memory();
        let subscriber = tracing_subscriber::registry().with(layer_for_provider(&provider));
        Self {
            provider,
            exporter,
            llm: InstrumentedLlm::new(Arc::new(ScriptedLlm)),
            _guard: tracing::subscriber::set_default(subscriber),
        }
    }

    fn llm_requests(&self) -> Option<u64> {
        self.provider.force_flush().expect("flush");
        counter_sum(&self.exporter, "cleverhans.llm.requests")
    }
}
