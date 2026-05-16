# Changelog

All notable changes to this project follow [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [1.0.0] - 2026-05-16

### Added
- **v0.2 obvious holes:** Linux+musllinux aarch64 wheels (sqlx native-tls swap), DuckDB primary-key / foreign-key introspection via `duckdb_constraints`, Python `sample_values=True` kwargs on `SchemaCache.from_url`, per-table `cache.refresh_table(name)`.
- **v0.3 bench teeth:** `run_ablation.py` (4-cell grid), `run_token_budget.py` (max_tokens 256/512/1024 sweep), `sentinel_corpus.py` + `run_sentinel.py` (Nokia-style sentinel corpus — qwen2.5-coder:3b baseline 0/5 → treatment 4/5), `run_bird_mini.py` (Anthropic/OpenAI stub).
- **v0.4 ergonomics:** `python/schemadex/embedding_resolve.py` Ollama-backed semantic fallback resolver, `cache.run_sql(url, sql, token_budget)` with markdown rendering, async API (`from_url_async`, `refresh_async`, `refresh_table_async`, `run_sql_async`), `schemadex-mcp --url ...` MCP server.
- **v0.5 backends:** MySQL via `sqlx-mysql`; BigQuery, Snowflake, MSSQL feature-gated scaffolds.
- **v0.6 production hardening:** real MSSQL (`tiberius`) + BigQuery (`gcp-bigquery-client`) + Snowflake (REST + RS256 JWT) backends; sampling on SQLite + MySQL + DuckDB; read-only `assert_readonly` + `allow_write=True` escape hatch on `run_sql`; process-wide connection pool reuse via `backends::shared_runner`.
- **v0.7 agent UX:** SQL pre-validation (`validate_sql`), error-to-hint wrapping (`hint_for_error`), synonym dictionary (`.schemadex/synonyms.yaml`), explicit JSON Schema MCP tool definitions + `schemadex-mcp --print-schemas`.
- **v0.8 ecosystem adapters:** LlamaIndex retriever, DSPy module, LiteLLM prompt builder, dbt-manifest cache source, IPython `%schemadex` magic.
- **v0.9 scale + ops:** streaming `run_sql` (BoxStream + byte-based token estimate), zstd-compressed cache (`database.json.zst` with legacy migration), OpenTelemetry export via new `otel` feature, MCP server `--metrics-port` health + Prometheus endpoints.
- **v1.0 stability commitment:** `docs/semver.md` (public API surface), `docs/migration-0.x-to-1.0.md`, `docs/deprecation.md`, `schemadex-api-audit` binary printing 25 locked-in symbols as JSON.
- Tracing spans on every backend method, PII redaction in sampling, slim/full backend feature flags on `schemadex-py`, PEP 740 OIDC publish path, mkdocs site at `amiyamandal1.github.io/schemadex`, query-plan-aware ranking in `describe_for_agent`, `schemadex-diff --from a.json --to b.json` CLI.

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
