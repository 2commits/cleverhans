//! OTEL metrics for the service (CLE-6): the lib crates emit structured
//! `tracing` events under the stable `cleverhans::telemetry::*` targets
//! (they carry no OTEL dependency, see [`cleverhans_core::telemetry`]);
//! this module — the only OTEL code in the workspace — converts those
//! events into OTLP-exported instruments.
//!
//! Runtime-gated: without an endpoint (config `[telemetry].otlp_endpoint`
//! or the standard `OTEL_EXPORTER_OTLP_ENDPOINT` env), nothing initializes
//! and the service behaves exactly as before.

use std::sync::Arc;
use std::time::Duration;

use opentelemetry::KeyValue;
use opentelemetry::metrics::{Counter, Histogram, Meter, MeterProvider as _, UpDownCounter};
use opentelemetry_sdk::metrics::{PeriodicReader, SdkMeterProvider};
use serde::Deserialize;
use tracing::field::{Field, Visit};
use tracing_subscriber::filter::EnvFilter;
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::{Layer, Registry};

use cleverhans_core::async_trait;
use cleverhans_core::error::LlmError;
use cleverhans_core::seams::{CompletionItem, CompletionRequest, CompletionStream, LlmProvider};

/// `[telemetry]` — OTLP push export (metrics only).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TelemetrySection {
    /// OTLP http endpoint, e.g. `http://localhost:4318`. Absent → falls
    /// back to `OTEL_EXPORTER_OTLP_ENDPOINT`; neither → telemetry off.
    #[serde(default)]
    pub otlp_endpoint: Option<String>,
    /// `service.name` resource attribute.
    #[serde(default)]
    pub service_name: Option<String>,
    /// Export interval (default 10 000 ms).
    #[serde(default)]
    pub export_interval_ms: Option<u64>,
}

/// The standard OTEL endpoint variable, used when the config omits one.
pub const ENDPOINT_ENV: &str = "OTEL_EXPORTER_OTLP_ENDPOINT";

impl TelemetrySection {
    /// The effective endpoint, honoring the standard OTEL env fallback.
    /// Startup-only: this reads the process environment on every call.
    #[must_use]
    pub fn endpoint(&self) -> Option<String> {
        resolve_endpoint(
            self.otlp_endpoint.as_deref(),
            std::env::var(ENDPOINT_ENV).ok().as_deref(),
        )
    }
}

/// Endpoint precedence, as a pure function: explicit config wins, the
/// standard env is the fallback, neither means telemetry stays off.
#[must_use]
pub fn resolve_endpoint(explicit: Option<&str>, env: Option<&str>) -> Option<String> {
    explicit.or(env).map(ToOwned::to_owned)
}

/// Errors from [`init`].
#[derive(Debug, thiserror::Error)]
pub enum TelemetryError {
    /// The OTLP exporter could not be built (typically a malformed
    /// endpoint) — startup fails loudly rather than exporting nowhere.
    #[error("otlp metric exporter: {0}")]
    Exporter(#[from] opentelemetry_otlp::ExporterBuildError),
}

/// Flushes and shuts the meter provider down on drop.
///
/// Hold it for the life of the process: dropping it exports whatever the
/// current interval has accumulated. A signal that kills the process
/// without unwinding skips this, losing at most one interval.
pub struct TelemetryGuard {
    provider: SdkMeterProvider,
}

impl Drop for TelemetryGuard {
    fn drop(&mut self) {
        if let Err(err) = self.provider.shutdown() {
            tracing::error!("telemetry shutdown: {err}");
        }
    }
}

/// The instrument set (one per metric in the CLE-6 reference table).
#[derive(Clone)]
pub struct Instruments {
    proposals: Counter<u64>,
    deliveries: Counter<u64>,
    delivery_duration: Histogram<f64>,
    execute_retries: Counter<u64>,
    sessions_active: UpDownCounter<i64>,
    session_duration: Histogram<f64>,
    llm_requests: Counter<u64>,
    llm_duration: Histogram<f64>,
}

impl Instruments {
    fn new(meter: &Meter) -> Self {
        Self {
            proposals: meter.u64_counter("cleverhans.proposals").build(),
            deliveries: meter.u64_counter("cleverhans.webhook.deliveries").build(),
            delivery_duration: meter
                .f64_histogram("cleverhans.webhook.delivery.duration")
                .with_unit("ms")
                .build(),
            execute_retries: meter
                .u64_counter("cleverhans.webhook.execute.retries")
                .build(),
            sessions_active: meter
                .i64_up_down_counter("cleverhans.sessions.active")
                .build(),
            session_duration: meter
                .f64_histogram("cleverhans.sessions.duration")
                .with_unit("ms")
                .build(),
            llm_requests: meter.u64_counter("cleverhans.llm.requests").build(),
            llm_duration: meter
                .f64_histogram("cleverhans.llm.duration")
                .with_unit("ms")
                .build(),
        }
    }
}

/// Initializes the OTLP meter provider from config; `None` when telemetry
/// is not configured.
///
/// # Errors
///
/// [`TelemetryError::Exporter`] when the endpoint cannot be turned into an
/// exporter.
pub fn init(
    section: &TelemetrySection,
) -> Result<Option<(TelemetryGuard, MetricsLayer)>, TelemetryError> {
    let Some(endpoint) = section.endpoint() else {
        return Ok(None);
    };
    use opentelemetry_otlp::WithExportConfig as _;
    let exporter = opentelemetry_otlp::MetricExporter::builder()
        .with_http()
        .with_endpoint(format!("{}/v1/metrics", endpoint.trim_end_matches('/')))
        .build()?;
    let reader = PeriodicReader::builder(exporter)
        .with_interval(Duration::from_millis(
            section.export_interval_ms.unwrap_or(10_000),
        ))
        .build();
    let provider = SdkMeterProvider::builder()
        .with_reader(reader)
        .with_resource(
            opentelemetry_sdk::Resource::builder()
                .with_service_name(
                    section
                        .service_name
                        .clone()
                        .unwrap_or_else(|| "cleverhans".to_owned()),
                )
                .build(),
        )
        .build();
    let meter = provider.meter("cleverhans-serve");
    let layer = MetricsLayer::new(Instruments::new(&meter));
    Ok(Some((TelemetryGuard { provider }, layer)))
}

/// Builds a layer over an existing provider — the test path (in-memory
/// exporter) and any embedder that manages its own OTEL setup.
#[must_use]
pub fn layer_for_provider(provider: &SdkMeterProvider) -> MetricsLayer {
    let meter = provider.meter("cleverhans-serve");
    MetricsLayer::new(Instruments::new(&meter))
}

/// The process subscriber: `filter` gates the log layer **only**.
///
/// Metrics deliberately sit outside it. An `EnvFilter` added to the
/// registry as a layer filters the whole stack, so `RUST_LOG=warn` — or any
/// crate-scoped directive, since the telemetry targets (`cleverhans::…`)
/// match no crate name — would silently zero every instrument while the
/// service looks healthy.
#[must_use]
pub fn subscriber(
    filter: EnvFilter,
    metrics_layer: Option<MetricsLayer>,
) -> impl tracing::Subscriber + Send + Sync {
    Registry::default()
        .with(tracing_subscriber::fmt::layer().with_filter(filter))
        .with(metrics_layer)
}

/// Converts `cleverhans::telemetry::*` tracing events into instrument
/// updates. Unknown targets and fields are ignored — the event contract is
/// forward-compatible.
pub struct MetricsLayer {
    instruments: Instruments,
}

impl MetricsLayer {
    fn new(instruments: Instruments) -> Self {
        Self { instruments }
    }
}

#[derive(Default)]
struct Fields {
    endpoint: Option<String>,
    outcome: Option<String>,
    state: Option<String>,
    action_id: Option<String>,
    phase: Option<String>,
    duration_ms: Option<u64>,
}

impl Visit for Fields {
    fn record_str(&mut self, field: &Field, value: &str) {
        match field.name() {
            "endpoint" => self.endpoint = Some(value.to_owned()),
            "outcome" => self.outcome = Some(value.to_owned()),
            "state" => self.state = Some(value.to_owned()),
            "action_id" => self.action_id = Some(value.to_owned()),
            "phase" => self.phase = Some(value.to_owned()),
            _ => {}
        }
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        if field.name() == "duration_ms" {
            self.duration_ms = Some(value);
        }
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        if field.name() == "duration_ms" {
            self.duration_ms = u64::try_from(value).ok();
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        // `%state` (Display) and similar arrive as debug records; `?value`
        // renders strings quoted, so strip one pair — not every quote.
        let rendered = format!("{value:?}");
        let unquoted = rendered
            .strip_prefix('"')
            .and_then(|inner| inner.strip_suffix('"'))
            .unwrap_or(&rendered);
        self.record_str(field, unquoted);
    }
}

impl<S: tracing::Subscriber> Layer<S> for MetricsLayer {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let target = event.metadata().target();
        let Some(kind) = target.strip_prefix("cleverhans::telemetry::") else {
            return;
        };
        let mut fields = Fields::default();
        event.record(&mut fields);

        match kind {
            "proposal" => {
                let mut attrs = vec![KeyValue::new(
                    "state",
                    fields.state.unwrap_or_else(|| "unknown".to_owned()),
                )];
                if let Some(action_id) = fields.action_id {
                    attrs.push(KeyValue::new("action_id", action_id));
                }
                self.instruments.proposals.add(1, &attrs);
            }
            "delivery" => {
                let attrs = vec![
                    KeyValue::new(
                        "endpoint",
                        fields.endpoint.unwrap_or_else(|| "unknown".to_owned()),
                    ),
                    KeyValue::new(
                        "outcome",
                        fields.outcome.unwrap_or_else(|| "unknown".to_owned()),
                    ),
                ];
                self.instruments.deliveries.add(1, &attrs);
                if let Some(duration) = fields.duration_ms {
                    // Duration keyed by endpoint only: outcome would split
                    // the histogram into low-volume series.
                    self.instruments
                        .delivery_duration
                        .record(duration as f64, &attrs[..1]);
                }
            }
            "delivery_retry" => {
                let attrs = fields.action_id.map_or_else(Vec::new, |action_id| {
                    vec![KeyValue::new("action_id", action_id)]
                });
                self.instruments.execute_retries.add(1, &attrs);
            }
            "session" => match fields.phase.as_deref() {
                Some("opened") => self.instruments.sessions_active.add(1, &[]),
                Some("closed") => {
                    self.instruments.sessions_active.add(-1, &[]);
                    if let Some(duration) = fields.duration_ms {
                        self.instruments
                            .session_duration
                            .record(duration as f64, &[]);
                    }
                }
                _ => {}
            },
            "llm" => {
                let attrs = vec![KeyValue::new(
                    "outcome",
                    fields.outcome.unwrap_or_else(|| "unknown".to_owned()),
                )];
                self.instruments.llm_requests.add(1, &attrs);
                if let Some(duration) = fields.duration_ms {
                    self.instruments.llm_duration.record(duration as f64, &[]);
                }
            }
            _ => {}
        }
    }
}

/// One model call, emitting its `cleverhans::telemetry::llm` event on drop.
///
/// Drop, not end-of-stream: the agent abandons a stream mid-generation
/// whenever the client goes away, and those calls — the slow and failing
/// ones — are exactly the ones a tail-of-stream emitter loses.
struct LlmSpan {
    started: std::time::Instant,
    errored: bool,
}

impl LlmSpan {
    fn start() -> Self {
        Self {
            started: std::time::Instant::now(),
            errored: false,
        }
    }

    /// Flips the outcome. A method, not a field write: a closure that only
    /// touches `span.errored` captures that field alone (disjoint capture),
    /// leaving the guard behind to drop at function exit and time nothing.
    fn mark_error(&mut self) {
        self.errored = true;
    }
}

impl Drop for LlmSpan {
    fn drop(&mut self) {
        tracing::info!(
            target: "cleverhans::telemetry::llm",
            outcome = if self.errored { "error" } else { "ok" },
            duration_ms = self.started.elapsed().as_millis() as u64,
            "llm call"
        );
    }
}

/// Times every model call, emitting `cleverhans::telemetry::llm` events —
/// the serve-side complement to the lib crates' events. Wraps the real
/// provider transparently; event emission is cheap and unconditional, the
/// OTEL conversion only happens when the layer is installed.
pub struct InstrumentedLlm {
    inner: Arc<dyn LlmProvider>,
}

impl InstrumentedLlm {
    /// Wraps `inner`, timing both the buffered and streaming calls.
    #[must_use]
    pub fn new(inner: Arc<dyn LlmProvider>) -> Self {
        Self { inner }
    }
}

#[async_trait]
impl LlmProvider for InstrumentedLlm {
    async fn complete(&self, request: CompletionRequest) -> Result<Vec<CompletionItem>, LlmError> {
        let mut span = LlmSpan::start();
        let result = self.inner.complete(request).await;
        if result.is_err() {
            span.mark_error();
        }
        result
    }

    async fn complete_stream(
        &self,
        request: CompletionRequest,
    ) -> Result<CompletionStream, LlmError> {
        use futures_util::StreamExt as _;
        let mut span = LlmSpan::start();
        let stream = match self.inner.complete_stream(request).await {
            Ok(stream) => stream,
            Err(err) => {
                span.mark_error();
                return Err(err);
            }
        };
        // The span moves into the stream, so it emits exactly once: when the
        // consumer exhausts the stream, or when it drops it early.
        Ok(Box::pin(stream.map(move |chunk| {
            if chunk.is_err() {
                span.mark_error();
            }
            chunk
        })))
    }
}
