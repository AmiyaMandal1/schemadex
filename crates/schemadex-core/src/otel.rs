//! OpenTelemetry integration. Behind the `otel` feature flag.
//!
//! The existing `#[tracing::instrument]` spans throughout schemadex-core flow
//! through `tracing-opentelemetry`'s layer into the OTLP exporter, so users
//! can ship spans to Jaeger / Honeycomb / any OpenTelemetry collector without
//! changing the call sites that emit them.
//!
//! See `docs/observability.md` for an end-to-end example.

use crate::error::{Result, SchemadexError};
use opentelemetry::trace::TracerProvider as _;
use opentelemetry::KeyValue;
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::{
    trace::{Config, TracerProvider},
    Resource,
};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

/// Install a tracing-subscriber registry with an OpenTelemetry layer pointed
/// at `otlp_endpoint`. After this returns, every `#[tracing::instrument]` span
/// in `schemadex-core` becomes an OTLP span on the named service.
///
/// `service_name` populates the `service.name` resource attribute; pick
/// something identifiable like `"schemadex"` or `"my-app-schemadex"`.
///
/// `otlp_endpoint` is the gRPC endpoint of an OpenTelemetry collector —
/// typically `http://localhost:4317` for a local collector running with the
/// default config. The function sets `OTEL_EXPORTER_OTLP_ENDPOINT` to the
/// provided value before building the exporter, so libraries that also read
/// the env var see the same endpoint.
///
/// Idempotency: this installs a global subscriber; calling it twice will
/// return an error on the second call because `tracing_subscriber` rejects
/// duplicate global subscribers.
pub fn init_otel(service_name: &str, otlp_endpoint: &str) -> Result<()> {
    // Surface the endpoint in the environment so downstream libraries that
    // also read OTEL_EXPORTER_OTLP_ENDPOINT pick up the same value.
    std::env::set_var("OTEL_EXPORTER_OTLP_ENDPOINT", otlp_endpoint);

    let exporter = opentelemetry_otlp::new_exporter()
        .tonic()
        .with_endpoint(otlp_endpoint)
        .build_span_exporter()
        .map_err(|e| SchemadexError::Other(format!("otlp exporter build failed: {e}")))?;

    let provider = TracerProvider::builder()
        .with_batch_exporter(exporter, opentelemetry_sdk::runtime::Tokio)
        .with_config(Config::default().with_resource(Resource::new(vec![KeyValue::new(
            "service.name",
            service_name.to_string(),
        )])))
        .build();

    let tracer = provider.tracer("schemadex");
    let otel_layer = tracing_opentelemetry::layer().with_tracer(tracer);

    // Install the provider globally so subprocesses / lazy spans pick it up.
    opentelemetry::global::set_tracer_provider(provider);

    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with(otel_layer)
        .try_init()
        .map_err(|e| SchemadexError::Other(format!("tracing subscriber init failed: {e}")))?;

    tracing::info!(
        service_name,
        otlp_endpoint,
        "schemadex.otel.initialized"
    );
    Ok(())
}
