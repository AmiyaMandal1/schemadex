# Architecture

```
+---------------------------+        +--------------------+
|  schemadex (Python pkg)   |        |  schemadex-core    |
|  - pure-Python adapters   |        |  (Rust crate)      |
|  - LangChain / LangGraph  +------->+  - introspector    |
|  - re-exports of _native  |        |  - cache           |
+-------------+-------------+        |  - resolve         |
              |                      |  - agent describe  |
              | maturin              +---------+----------+
              v                                |
+---------------------------+                  v
|  schemadex._native        |   +-----------------------------+
|  (cdylib, PyO3 0.22)      |   |  backends (feature-gated)   |
|  - SchemaCache            |   |  postgres / sqlite / duckdb |
|  - ResolveResult          |   +-----------------------------+
+---------------------------+
```

## Why a workspace split?

- `schemadex-core` is a pure-Rust crate. Other Rust tools (a SQL formatter,
  a CLI) can depend on it without pulling PyO3.
- `schemadex-py` is the PyO3 bridge. It builds as `cdylib` → `_native.so`
  shipped inside the `schemadex` wheel. Pure-Python adapters live under
  `python/schemadex/` so we can iterate on framework integrations without
  recompiling Rust.

## Cache layout

```
$CACHE_DIR/schemadex/<url_hash>/database.json
```

`url_hash` is a SHA-256 truncated to 16 hex characters of the database URL
**with credentials stripped**. The cache file is a JSON envelope:

```json
{"saved_at_unix": 1715000000, "database": { ... }}
```

Invalidation:

1. **TTL** — if `now - saved_at_unix > ttl`, refresh.
2. **DDL fingerprint** — at refresh time, compare per-table `ddl_hash`.
   Tables whose hash matches are left alone; only changed tables get
   re-introspected.

## Token-budgeted describe

See `crate::agent::describe_for_agent`. Truncation hierarchy in order:

1. Drop samples.
2. Drop column / table comments.
3. Drop foreign keys.
4. Drop columns past ordinal 8 per table.
5. Drop lowest-ranked tables (relevance against `hint`).

`tiktoken-rs` is the token counter; we use the `cl100k_base` encoding so the
estimate is accurate for GPT-4o-family and Claude-class tokenizers within a
few percent.

## Backend quirks

| Backend  | Schemas  | PK/FK from   | Sampling                 |
|----------|----------|--------------|--------------------------|
| Postgres | yes      | `information_schema` + `pg_catalog` | full top-K + percentiles |
| SQLite   | no       | `PRAGMA table_info`, `PRAGMA foreign_key_list` | not yet wired |
| DuckDB   | yes (`main`) | omitted (varies by version) | not yet wired |

## Observability

`schemadex-core` emits structured `tracing` spans on every externally-visible
entry point — `SchemaCache::{from_introspector, refresh, refresh_table,
run_sql}` at `info`, plus the per-backend trait methods (`tables`, `columns`,
`primary_key`, `foreign_keys`, `run_sql`) at `debug`. Spans carry the backend
name, URL hash (credential-stripped), table identity, and result sizes so a
log scan tells you what the cache did without reading rows.

Wire up a subscriber once at process start (any `tracing_subscriber`
configuration works) and drive the filter via `RUST_LOG`:

```bash
# normal operation: high-signal cache events plus warnings from sqlx
RUST_LOG=schemadex=info,sqlx=warn cargo run

# debug a slow refresh: include per-table backend calls
RUST_LOG=schemadex=debug cargo run
```

Filtering to a single backend uses the span target prefix
`schemadex_core::backends::<backend>`:

```bash
# only postgres backend spans
RUST_LOG=schemadex_core::backends::postgres=debug,schemadex=info cargo run

# only the cache, no backend chatter
RUST_LOG=schemadex_core::cache=info cargo run
```

Row streams from `run_sql` are deliberately `skip`-ped from the span fields
to keep query results out of logs.
