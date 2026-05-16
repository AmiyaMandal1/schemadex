# Changelog

All notable changes to this project follow [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- Workspace layout: `schemadex-core` (Rust) + `schemadex-py` (PyO3 bindings as `schemadex._native`).
- `SchemaIntrospector` trait with Postgres, SQLite, and DuckDB backends behind feature flags.
- `SchemaCache` with on-disk persistence at `~/.cache/schemadex/<url-hash>/`, TTL + DDL fingerprint invalidation, parallel refresh via `tokio::join_all`.
- Sample-value collection: top-K + null fraction for categorical columns, min/max/p50/p95/p99 for numeric columns.
- Sentinel detection: flag any value strictly above 40% frequency.
- Fuzzy column resolution (`resolve_column`) backed by Jaro-Winkler.
- Token-budgeted `describe_for_agent` rendering with a documented truncation hierarchy.
- LangChain `Tool` adapter and LangGraph node adapter (examples).
- CI: `cargo fmt`/`clippy`, `cargo test` on stable + MSRV 1.75, multi-arch wheel builds via maturin.
