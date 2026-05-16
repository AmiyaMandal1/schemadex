# Observability

schemadex-core emits `tracing` spans throughout the introspection, refresh,
and `run_sql` paths. The `otel` feature flag adds an OpenTelemetry exporter so
those spans can be shipped to any OTLP-speaking backend — Jaeger, Honeycomb,
Tempo, Datadog, or a vanilla [opentelemetry-collector][collector].

## Enabling the feature

```toml
[dependencies]
schemadex-core = { version = "0.1", features = ["otel"] }
```

## Wiring it up

Call `init_otel` once at process start, before constructing any `SchemaCache`:

```rust
use schemadex_core::init_otel;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_otel("schemadex", "http://localhost:4317")?;

    // ... build your SchemaCache, run queries, etc.
    // Every #[tracing::instrument] span in schemadex-core is now an OTLP span.
    Ok(())
}
```

`service_name` populates the `service.name` resource attribute. `otlp_endpoint`
is the gRPC endpoint of an OpenTelemetry collector (default `4317`).

The function also writes `OTEL_EXPORTER_OTLP_ENDPOINT` into the environment so
libraries that read it directly see the same endpoint.

## Running a local collector

A minimal `otelcol` config that accepts OTLP/gRPC and prints traces to stdout:

```yaml
# otel-collector-config.yaml
receivers:
  otlp:
    protocols:
      grpc:
        endpoint: 0.0.0.0:4317

processors:
  batch: {}

exporters:
  logging:
    loglevel: debug

service:
  pipelines:
    traces:
      receivers: [otlp]
      processors: [batch]
      exporters: [logging]
```

Run it:

```bash
docker run --rm -p 4317:4317 \
    -v $(pwd)/otel-collector-config.yaml:/etc/otelcol/config.yaml \
    otel/opentelemetry-collector:latest
```

## Span names you'll see

The instrumented entry points include (non-exhaustive):

- `schema_cache.from_introspector`
- `schema_cache.refresh`
- `schema_cache.refresh_table`
- `schema_cache.run_sql`
- `schema_cache.run_sql_unchecked`
- `schema_cache.run_sql_streaming`
- `schema_cache.run_sql_validated`
- `<backend>.tables`, `<backend>.columns`, `<backend>.run_sql`, …

Each carries structured fields like `backend`, `url_hash`, `sql_len`,
`token_budget`, `rows`, and `truncated`. These show up as span attributes in
your tracing backend.

## Shipping to Jaeger / Honeycomb / Datadog

All three accept OTLP/gRPC on `4317` either directly or via the collector.
Point `init_otel` at the right endpoint and you're done — no schemadex-side
configuration changes.

[collector]: https://opentelemetry.io/docs/collector/
