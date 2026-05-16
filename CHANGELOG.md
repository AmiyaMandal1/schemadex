# Changelog

All notable changes to this project follow [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Documentation
- v1.0 stability commitment: added `docs/semver.md` (public API surface +
  what counts as breaking), `docs/migration-0.x-to-1.0.md` (placeholder to
  fill in as 1.0 PRs land), and `docs/deprecation.md` (deprecation
  process).
- New `schemadex-api-audit` binary in `schemadex-core` that prints the
  locked-in public symbol list as JSON for pre-1.0 review.

## [0.1.1] - 2026-05-16

### Fixed
- PyPI publish failed for the sdist on 0.1.0 because `License-File LICENSE-APACHE does not exist in distribution file`. Switched `pyproject.toml` to PEP 639 (`license = "MIT OR Apache-2.0"` + `license-files = ["LICENSE-MIT", "LICENSE-APACHE"]`) and added an explicit `[tool.maturin].include` so the LICENSE files land inside the sdist tarball. Also publishes the macOS aarch64 wheel that was queued behind the failing sdist on 0.1.0.

## [0.1.0] - 2026-05-16

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
