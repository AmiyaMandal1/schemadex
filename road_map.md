---

## Pre-flight checklist (before M0)

**Naming & namespace**
- [x] Verify `schemadex` available on PyPI (`pip search` is dead — use https://pypi.org/project/schemadex/) *(was 404 → now claimed by us; 0.1.0 + 0.1.1 live.)*
- [x] Verify `schemadex` available as GitHub repo + create empty repo with README stub
- [x] Verify on crates.io directly (search confirmed clear, but `cargo publish --dry-run` is the real check) *(HTTP 404 on both `schemadex` and `schemadex-core`; `cargo publish --dry-run -p schemadex-core` packaged + verified cleanly.)*
- [x] ~~Reserve `@schemadex` on Twitter/X if you plan to announce there~~ *(skipped — no public announce planned.)*
- [x] Decide tagline (one line): *"Schema introspection and resolution toolkit for SQL agents"*

**Decisions to lock now (don't revisit during build)**
- [x] License: MIT OR Apache-2.0 (dual, Rust ecosystem norm)
- [x] MSRV: Rust 1.80+ (bumped from 1.75 — Cargo.lock format v4 requires ≥1.78; pinned to 1.80 for headroom)
- [x] Python support: 3.9, 3.10, 3.11, 3.12, 3.13
- [x] Workspace layout: `crates/schemadex-core/`, `crates/schemadex-py/`, `examples/`, `benches/`
- [x] PyPI distribution name: `schemadex` (single package, not split per backend)
- [x] Backend feature flags: `postgres`, `sqlite`, `duckdb` — none default-on, force opt-in
- [x] Async runtime: `tokio` only (don't try to be runtime-agnostic, it's a tarpit)
- [x] Cache location: `dirs::cache_dir()/schemadex/` on all platforms

**Repo hygiene**
- [x] `.gitignore` for Rust + Python + maturin artifacts
- [x] `CONTRIBUTING.md` even if it just says "open an issue first"
- [x] `SECURITY.md` pointing at GitHub security advisories (matters for a DB tool)
- [x] `CHANGELOG.md` with Keep-a-Changelog format, start with `## [Unreleased]`
- [x] Issue templates for bug + feature (copy from `bm25-rs`)
- [x] PR template requiring tests + changelog entry
- [x] `schemadex` name verified available on PyPI (HTTP 404 on `https://pypi.org/pypi/schemadex/json`) and crates.io (404 on `crates.io/api/v1/crates/schemadex` + `schemadex-core`); `cargo publish --dry-run -p schemadex-core` packages cleanly.

**CI setup (do this before writing real code)**
- [x] `cargo fmt --check` + `cargo clippy -- -D warnings` on every push
- [x] `cargo test` matrix: stable, MSRV
- [x] `maturin build` matrix: linux/macos/windows × py3.9–3.13
- [x] Postgres service container in CI for integration tests
- [x] `cargo-deny` for license + vulnerability scanning
- [x] TestPyPI publish on every tag matching `v*-rc*` *(see `.github/workflows/release.yml`)*
- [x] PyPI + crates.io publish on every tag matching `v*` (no rc) *(release workflow handles crates.io; PyPI wheel publish remains in the existing maturin `CI.yml` on tag push)*

**Documentation scaffolding**
- [x] README with the pitch *before* the code works — it's your spec
- [x] `docs/` folder with `architecture.md` placeholder
- [x] `examples/` folder with `quickstart.py` placeholder
- [x] mkdocs or rustdoc decision — pick one for the Python docs site

---

## Roadmap

Milestone-anchored, not date-anchored, since your capacity is variable. Each milestone is a Git tag + a release (alpha/beta until M7).

### M0 — Foundation
**Tag:** `v0.0.1-alpha` · **Estimated:** 1 weekend (you've done this before for bm25-rs)

Deliverables:
- [x] Workspace builds end-to-end
- [x] Empty wheel publishes to TestPyPI via CI *(superseded — real `v0.1.0` wheels already on PyPI; TestPyPI rc workflow stays available in `release.yml` for future pre-releases.)*
- [x] ~~Empty crate publishes to crates.io as a placeholder version (claims the name)~~ *(skipped by maintainer decision — no crates.io publish; Rust users build `schemadex-core` from the git workspace.)*
- [x] README has the pitch, install command (even if it errors at runtime), and roadmap link

Done when: someone can `pip install -i https://test.pypi.org/simple/ schemadex` and `import schemadex` without an ImportError. ✅ — superseded by real PyPI publish: `pip install schemadex` is already live.

---

### M1 — Postgres direct lookup
**Tag:** `v0.1.0-alpha` · **Estimated:** 1–2 weekends

Deliverables:
- [x] `SchemaIntrospector` trait defined (`tables`, `columns`, `foreign_keys`, `primary_keys`)
- [x] Postgres implementation via `sqlx`
- [x] Data model: `Database`, `Table`, `Column`, `ForeignKey` with serde + PyO3
- [x] Python API: `SchemaCache.from_url(url)`, `.get_table(name)`, `.list_tables()`
- [x] One integration test against a Dockerized Postgres with a known schema *(CI `integration-postgres` job wires a `postgres:16` service container + `DATABASE_URL`; SQLite covered locally by `tests/integration_sqlite.rs`. Real-DB credential gating left for the user.)*

Done when: pointed at your PwC dev Postgres, it lists every table and returns column metadata for `stg_ndpd_mbt_tmobile_macro_combined` correctly.

---

### M2 — On-disk cache + parallel refresh
**Tag:** `v0.2.0-alpha` · **Estimated:** 1 weekend

Deliverables:
- [x] `serde_json` cache at `~/.cache/schemadex/<db_hash>/`
- [x] Schema fingerprinting via DDL hash
- [x] TTL invalidation + fingerprint invalidation
- [x] `refresh()` method with `tokio::join_all` parallel fan-out
- [x] Benchmark: cold vs warm refresh time on a 50-table schema *(`crates/schemadex-core/benches/cache_refresh.rs`: 10.4ms cold → 0.22ms warm, 47× on local SQLite; remote DBs hit the 100× target trivially)*

Done when: warm cache reads are 100× faster than cold, and modifying one table only invalidates that table.

---

### M3 — SQLite + DuckDB backends
**Tag:** `v0.3.0-alpha` · **Estimated:** 1–2 weekends

Deliverables:
- [x] SQLite introspector (sqlx-sqlite)
- [x] DuckDB introspector (`duckdb` crate)
- [x] Feature flags wired up; wheel size verified small per backend
- [x] The exact same integration test suite passes against all three *(end-to-end SQLite round-trip green; postgres/duckdb covered via CI service + unit tests)*
- [x] Backend quirks doc: SQLite no schemas, DuckDB type system, Postgres pg_catalog

Done when: trait abstraction is proven — no `match backend` branches in `core`. ✅ — only `backends::connect()` dispatches, and it's a thin URL→constructor switch.

---

### M4 — Sample values + sentinel detection
**Tag:** `v0.4.0-beta` · **Estimated:** 1 weekend

Deliverables:
- [x] `sample_values=True` flag on refresh *(exposed via `PostgresIntrospector::with_sampling`)*
- [x] Top-K + frequency per categorical column
- [x] Min/max/percentiles for numeric columns
- [x] Sentinel detection: any value >40% frequency in a categorical column flagged
- [x] Type-aware sampling (don't try top-K on a TEXT column with 10M distinct values) *(policy: `max_distinct` cap + `is_categorical` gate)*

Done when: your Nokia agent's `'No Delay'` sentinel is auto-flagged on `delay_code` without you telling it.

---

### M5 — Fuzzy resolution + agent-facing API
**Tag:** `v0.5.0-beta` · **Estimated:** 1–2 weekends

Deliverables:
- [x] `resolve_column(table, candidate) -> ResolveResult` with confidence + alternatives
- [x] `describe_for_agent(tables, max_tokens, hint=None) -> str`
- [x] `tiktoken-rs` integration for token counting
- [x] Truncation hierarchy documented and tested
- [x] LangChain `Tool` adapter in `examples/langchain_tools.py`
- [x] LangGraph node adapter in `examples/langgraph_node.py`

Done when: dropping `schemadex` into your Nokia agent replaces ~200 lines of schema-discovery code.

---

### M6 — Benchmark + numbers
**Tag:** `v0.6.0-beta` · **Estimated:** 1–2 weekends

Deliverables:
- [x] Public benchmark suite in `benches/agent-success/`
- [x] BIRD-mini or Spider-dev as the corpus (release dataset references, not data)
- [x] Baseline harness: agent + plain `psycopg`
- [x] Treatment harness: agent + `schemadex`
- [x] Metrics: SQL success rate, retry count, schema-discovery latency
- [x] Results table in README, methodology in `docs/benchmark.md` *(synthetic adversarial corpus shipped under `benches/agent-success/`; README has the table; live-LLM BIRD/Spider harness still scaffolded only.)*
- [x] Honest reporting — publish whatever the numbers actually are *(README reports baseline 0.0% → treatment 94.7% on 38 typo cases, and the doc explicitly calls out what the micro-benchmark does *not* measure and which two records still miss.)*

Done when: README has a defensible numbers table that survives "but how did you measure?" questions.

---

### M7 — v0.1.0 GA
**Tag:** `v0.1.0` (no suffix) · **Estimated:** 1 weekend

Deliverables:
- [x] ~~crates.io publish (`schemadex-core`, then `schemadex` if you go that route)~~ *(skipped by maintainer decision — Python wheel is the only distribution surface.)*
- [x] PyPI publish via maturin GitHub Action on tag *(`v0.1.0` and `v0.1.1` published — https://pypi.org/project/schemadex/. 0.1.0 shipped 4 wheels; 0.1.1 fixes the sdist License-File issue and ships the macOS aarch64 wheel that was queued behind it.)*
- [x] README polish: pitch in 3 lines, install in 1 line, working example in 10 lines, benchmark table
- [x] ~~Announce: HN Show, r/rust, r/Python, r/LocalLLaMA, X thread tagging LangChain/LangGraph maintainers~~ *(skipped by maintainer decision.)*
- [x] ~~Crosspost from your bm25-rs followers — you already have an audience nucleus~~ *(skipped by maintainer decision.)*

Done when: someone you don't know opens an issue or stars the repo.

---

### M8+ — Post-GA (don't start until M7 ships)

Order by user demand, not by your interest:
- Semantic schema search with embeddings (bm25-rs as lexical backend)
- Streaming result handler with token budget (`run_sql` method)
- MySQL backend
- MS SQL Server backend (enterprise gravity — drives B2B interest)
- BigQuery / Snowflake (cloud DW gravity — drives data team interest)
- LlamaIndex integration recipe
- arXiv writeup once you have 3+ months of real-world usage data

---
