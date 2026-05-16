# Usage

A practical, end-to-end tour of the schemadex Python API. Every section
below is runnable as written — paste a snippet into a REPL with the
package installed and it should produce sensible output against your own
database.

Higher-level design context lives in
[architecture.md](architecture.md). For the resolution-path numbers cited
here, see [benchmark.md](benchmark.md).

## 1. Install

The default wheel ships every backend (Postgres, SQLite, MySQL, DuckDB,
BigQuery, Snowflake, MSSQL) plus the agent-facing describe / resolve /
run-SQL surface.

```bash
# Everything
pip install schemadex

# With one of the optional framework adapters
pip install "schemadex[mcp]"          # MCP server entry point
pip install "schemadex[langchain]"    # langchain_core.tools.Tool factory
pip install "schemadex[langgraph]"    # LangGraph node helper
pip install "schemadex[llamaindex]"   # LlamaIndex retriever
pip install "schemadex[dspy]"         # DSPy module
pip install "schemadex[litellm]"      # LiteLLM message builder

# Or several at once
pip install "schemadex[mcp,langchain,langgraph]"
```

### Building a slim wheel from source

The published wheel includes every backend; the binary is therefore
heavy. Users who need a smaller artifact can build one locally with
`maturin`, picking only the backends they care about (full recipe in
[slim-wheels.md](slim-wheels.md)):

```bash
# Postgres + SQLite, no DuckDB / BigQuery / Snowflake / MSSQL / MySQL
git clone https://github.com/AmiyaMandal1/schemadex
cd schemadex
maturin build --release --no-default-features \
    --features schemadex-py/postgres,schemadex-py/sqlite \
    --out dist
pip install dist/schemadex-*.whl
```

### Rust git dependency

`schemadex-core` is not on crates.io yet; pull it directly from GitHub:

```toml
# Cargo.toml
[dependencies]
schemadex-core = {
    git = "https://github.com/AmiyaMandal1/schemadex",
    features = ["postgres", "sqlite", "duckdb_backend"],
}
```

### Verify the install

```bash
python -c "import schemadex; print(schemadex.__version__)"
```

## 2. Connecting to a database

`SchemaCache.from_url(url)` is the single entry point. The URL scheme
selects the backend; credentials follow the conventions of the
underlying driver.

| Backend    | URL scheme                                                | Credentials |
|------------|-----------------------------------------------------------|-------------|
| Postgres   | `postgres://user:pass@host:port/db`                       | password in URL or `PGPASSWORD` env |
| SQLite     | `sqlite:///abs/path/to.db` or `sqlite://./rel.db?mode=rwc`| none |
| MySQL      | `mysql://user:pass@host:port/db`                          | password in URL |
| DuckDB     | `duckdb://` (in-memory) or `duckdb:///abs/path.duckdb`    | none |
| MSSQL      | `mssql://user:pass@host:port/db`                          | `?encrypt=true` / `?trust_cert=true` query params |
| BigQuery   | `bigquery://project[/dataset]`                            | `GOOGLE_APPLICATION_CREDENTIALS` env or ADC (`gcloud auth application-default login`) |
| Snowflake  | `snowflake://account/database[/schema]`                   | `SNOWFLAKE_USER`, `SNOWFLAKE_PRIVATE_KEY_PATH`, optional `SNOWFLAKE_WAREHOUSE`, `SNOWFLAKE_ROLE` |

One-liners per backend:

```python
from schemadex import SchemaCache

# Postgres
cache = SchemaCache.from_url("postgres://app:secret@db.internal:5432/orders")

# SQLite (absolute path)
cache = SchemaCache.from_url("sqlite:///var/lib/app/data.db")

# MySQL
cache = SchemaCache.from_url("mysql://app:secret@db.internal:3306/orders")

# DuckDB (file)
cache = SchemaCache.from_url("duckdb:///tmp/analytics.duckdb")

# MSSQL with self-signed cert
cache = SchemaCache.from_url(
    "mssql://sa:Pass1!@sqlserver:1433/orders?encrypt=true&trust_cert=true"
)

# BigQuery (ADC must be set up first: `gcloud auth application-default login`)
cache = SchemaCache.from_url("bigquery://my-project/analytics")

# Snowflake (key-pair JWT)
# export SNOWFLAKE_USER=...; export SNOWFLAKE_PRIVATE_KEY_PATH=/path/to/key.p8
cache = SchemaCache.from_url("snowflake://my-account/ANALYTICS/PUBLIC")
```

## 3. Basic cache operations

Once a `SchemaCache` is built, everything else is local:

```python
from schemadex import SchemaCache

cache = SchemaCache.from_url("postgres://app:secret@db/orders")

# List every cached table (qualified name when the backend has schemas).
print(cache.list_tables())
# ['public.customers', 'public.orders', 'public.shipments']

# Get the full structured record for one table.
table = cache.get_table("public.orders")
print(table["columns"][0])
# {'name': 'id', 'data_type': 'integer', 'nullable': False, ...}

# Per-database DDL fingerprint (None if no fingerprint was computed).
print(cache.fingerprint())
# 'sha256:9f2c…'

# Where this cache lives on disk.
print(cache.cache_path())
# '/Users/you/Library/Caches/schemadex/9b21c4e8…/database.json.zst'

# Full dump as a JSON string (suitable for diffing or shipping to a peer).
blob = cache.to_json()
```

The first call to `from_url(url)` writes a zstd-compressed envelope to
`~/.cache/schemadex/<url-hash>/database.json.zst` (on macOS the path is
under `~/Library/Caches/`, on Linux `$XDG_CACHE_HOME` or `~/.cache`).
Subsequent calls within the TTL (24 hours by default; override with
`ttl_seconds=`) read the envelope back instead of hitting the database.
The architecture doc has the full cache-layout spec
([architecture.md](architecture.md#cache-layout)).

## 4. Refreshing the cache

When DDL changes you can re-introspect without re-paying the cost on
every table:

```python
# Re-introspect everything. Tables whose DDL hash is unchanged are
# left alone; only changed tables are rewritten.
changed, unchanged = cache.refresh("postgres://app:secret@db/orders")
print(f"{len(changed)} changed, {len(unchanged)} unchanged")

# Re-introspect a single table by qualified or bare name.
changed, unchanged = cache.refresh_table(
    "postgres://app:secret@db/orders",
    "public.orders",
)
```

Both calls return a `(changed, unchanged)` tuple of qualified table
names. The DDL fingerprint check happens inside the Rust core — if a
table's hash matches the persisted snapshot, it doesn't get rewritten.

## 5. Resolving fuzzy column names

This is the primary friction-removal feature. The agent says
`customer_idd`; you want `customer_id`.

```python
r = cache.resolve("public.orders", "customer_idd")
print(r.matched)        # 'customer_id'
print(r.confidence)     # 0.94
print(r.alternatives)   # [('customer_uuid', 0.71), ('id', 0.55)]
```

Scoring model:

- Base score is Jaro-Winkler similarity between candidate and every
  column name on the table.
- `confidence == 1.0` means an exact (case-insensitive) match.
- `alternatives` contains up to three runner-up `(name, score)` pairs.

In practice, treat anything `>= 0.85` as "trust the match," anything
between `0.70` and `0.85` as "ask the user," and lower than `0.70` as
"the candidate is probably wrong; fall back to listing columns."

### Embedding fallback

For semantic misses Jaro-Winkler can't bridge (`review_body` ↔ `body`,
`state` ↔ `status`), `resolve_with_embedding` re-ranks the candidates by
cosine similarity of embeddings produced by a local Ollama model:

```python
from schemadex import SchemaCache, resolve_with_embedding

cache = SchemaCache.from_url("postgres://app:secret@db/reviews")
r = resolve_with_embedding(
    cache,
    "users",
    "review_body",
    threshold=0.85,                    # only embed if lexical < 0.85
    model="nomic-embed-text-v2-moe",
    ollama_url="http://localhost:11434",
)
print(r.matched, r.confidence)
```

Requires `ollama serve` to be running locally and the requested model to
be pulled (`ollama pull nomic-embed-text-v2-moe`). If Ollama is
unreachable or the model is missing, `resolve_with_embedding` logs a
warning to stderr and returns the original lexical result — it never
raises.

### Synonym dictionary

For domain-specific aliases the lexical scorer doesn't know about
(`amount_cents` is the project's name for `total`, etc.) provide a YAML
synonym map:

```yaml
# .schemadex/synonyms.yaml
public.orders:
  total: amount_cents
  shipped_at: dispatched_at
public.customers:
  email: contact_email
```

```python
# Option A: load once, reuse on every resolve call.
cache.load_synonyms(".schemadex/synonyms.yaml")
r = cache.resolve("public.orders", "total",
                  synonyms_path=".schemadex/synonyms.yaml")
print(r.matched)   # 'amount_cents'
```

The parsed synonym map is cached on the `SchemaCache` instance, so
repeated calls with the same `synonyms_path` don't re-read the file.

## 6. Token-budgeted schema descriptions

The describe API renders the cache into a chunk of text small enough to
fit in an LLM prompt:

```python
prompt, tokens = cache.describe_for_agent(
    max_tokens=1500,
    hint="orders by region",
    include_samples=True,
    include_foreign_keys=True,
)
print(f"-- {tokens} tokens --")
print(prompt)
```

Key behaviors:

- `hint` biases the per-table relevance score. Tables that match the
  hint are kept; low-ranked tables are dropped first when the budget is
  tight.
- `max_tokens` is counted with `tiktoken` (`cl100k_base` encoding), the
  same tokenizer the GPT-4o-family and Claude-class models use, so the
  estimate is within a few percent of the model's own count.
- When the rendered output exceeds the budget, the truncation hierarchy
  fires in this order:
  1. drop sample values,
  2. drop column / table comments,
  3. drop foreign keys,
  4. drop columns past ordinal 8 per table,
  5. drop lowest-ranked tables.

You can also restrict the output to specific tables — useful when the
agent already knows roughly what it wants:

```python
prompt, tokens = cache.describe_for_agent(
    max_tokens=800,
    tables=["public.orders", "public.customers"],
)
```

For very tight budgets (under ~512 tokens on a 50-table schema), see the
token-budget stress run in [benchmark.md](benchmark.md).

## 7. Running SQL

Schemadex includes a read-only SQL runner with built-in markdown
rendering. The connection pool is created lazily per URL and shared
across calls, so the first invocation pays the connect cost and the
rest don't.

```python
# Blocking call — returns once the entire result set has been rendered.
text, tokens = cache.run_sql(
    "postgres://app:secret@db/orders",
    "SELECT id, email FROM customers ORDER BY id LIMIT 50",
    token_budget=1024,
)
print(text)
# | id | email           |
# | --- | --- |
# | 1 | a@example.com |
# ...

# Streaming variant — the backend stops pulling rows as soon as the
# rendered table would exceed `token_budget`. Use this for queries that
# might return millions of rows.
text, tokens = cache.run_sql_streaming(
    "postgres://app:secret@db/orders",
    "SELECT * FROM big_table",
    token_budget=512,
)
```

Safety model:

- Both functions parse the SQL through `assert_readonly` first, which
  rejects everything except `SELECT` / `WITH` / `EXPLAIN` / `SHOW` /
  `DESCRIBE` / `DESC`.
- `run_sql(..., allow_write=True)` skips the read-only guard. Only do
  this if you have already validated the SQL yourself — `DELETE`,
  `DROP`, and `UPDATE` will reach the database.
- `run_sql_streaming` always enforces the read-only check; there is no
  `allow_write` escape hatch on the streaming path.

The output is a markdown pipe-table. If rows had to be dropped to fit
the budget, the renderer appends `_(truncated to N rows)_` so the agent
knows it didn't see everything.

## 8. SQL pre-validation and error-to-hint

`validate_sql` runs the cached schema over a query before you execute
it. It is a heuristic, regex-driven check — not a full SQL parser — but
it catches the typos LLM SQL agents actually emit.

```python
issues = cache.validate_sql("SELECT emial FROM users")
print(issues)
# [{
#     'kind': 'unknown_column',
#     'table': 'users',
#     'identifier': 'emial',
#     'suggestion': 'email',
#     'confidence': 0.95,
# }]
```

`hint_for_error` runs the same logic in reverse: given a raw database
error message, it pulls out the likely-real identifier and emits a
structured hint the agent can retry with:

```python
hint = cache.hint_for_error('column "emial" does not exist')
print(hint)
# {
#     'kind': 'unknown_column',
#     'table': None,
#     'original_identifier': 'emial',
#     'suggested_identifier': 'email',
#     'confidence': 0.95,
#     'human_message': "column 'emial' does not exist — did you mean 'email'?",
# }
```

`hint_for_error` returns `None` if the error text doesn't match a known
pattern.

## 9. Sampling and sentinel detection

Sampling collects top-K values and percentiles per column, then flags
any value covering more than 40% of the column as a sentinel — the
common case being unhelpful placeholders like `'No Delay'` or `'N/A'`
that an agent shouldn't filter on naively.

```python
cache = SchemaCache.from_url(
    "postgres://app:secret@db/outages",
    sample_values=True,
    sample_top_k=10,
    sample_sentinel_threshold=0.4,
    sample_rows=10000,
)
table = cache.get_table("public.outages")
delay = next(c for c in table["columns"] if c["name"] == "delay_code")
print(delay["sample"]["sentinel"])
# ('No Delay', 0.80)
```

Sampling is supported on Postgres, SQLite, MySQL, and DuckDB.

PII redaction is enabled by default. Columns named `email`, `phone`,
`ssn`, `password`, etc., are skipped at sample time — the `sample` field
on those columns is `None`. The same skip applies to columns whose
comment carries a "PII" / "personally identifiable" marker. To override,
construct a custom `SamplingPolicy` in Rust and clear `redaction`; the
Python binding exposes only the safe defaults.

## 10. Async API

The async variants share the same tokio runtime as the sync API and
don't block the event loop:

```python
import asyncio
from schemadex import from_url_async, run_sql_async, refresh_async

URL = "postgres://app:secret@db/orders"

async def main():
    cache = await from_url_async(URL)

    text, tokens = await run_sql_async(
        cache,
        URL,
        "SELECT id, email FROM customers LIMIT 50",
        token_budget=1024,
    )

    changed, unchanged = await refresh_async(cache, URL)

asyncio.run(main())
```

`refresh_table_async` is the single-table sibling of `refresh_async`.
All async functions accept the same sampling kwargs as their sync
counterparts.

## 11. MCP server

Install with `pip install "schemadex[mcp]"`. The `schemadex-mcp`
console script speaks the Model Context Protocol over stdio:

```bash
schemadex-mcp --url "sqlite:///path/to/db.sqlite"
```

Wire it into Claude Code by adding to `~/.claude/mcp.json`:

```json
{
  "mcpServers": {
    "schemadex": {
      "command": "schemadex-mcp",
      "args": ["--url", "sqlite:///path/to/db.sqlite"]
    }
  }
}
```

The agent then has these tools, with no extra wiring:

| Tool | Description |
|------|-------------|
| `list_tables` | List every table in the connected database. |
| `describe_for_agent` | Render a token-budgeted schema description, optionally biased by a free-text `hint`. |
| `resolve_column` | Fuzzy-resolve a candidate column name on a table. Returns matched + confidence + up to three alternatives. |
| `run_sql` | Run a read-only SQL query and return a markdown-rendered result table that fits inside `token_budget`. |
| `validate_sql` | Pre-validate a SQL query against the cached schema. Returns a list of issues; an empty list means the query references only known identifiers. |
| `hint_for_error` | Wrap a raw database error message in a structured hint (`{kind, original_identifier, suggested_identifier, human_message}`). |

Operational flags:

```bash
# Expose /health and /metrics (Prometheus text format) on port 9100.
schemadex-mcp --url ... --metrics-port 9100

# Print the registered tool catalog as JSON and exit. Useful for
# agents that don't speak MCP but want the JSON Schema for each tool.
schemadex-mcp --url ... --print-schemas
```

The `/metrics` endpoint exposes `schemadex_cache_tables`,
`schemadex_introspection_seconds_total`, `schemadex_run_sql_calls_total`,
and `schemadex_run_sql_errors_total`.

## 12. Agent framework adapters

Each adapter ships as a runnable example under `examples/`. Copy the
file into your own codebase or import it directly.

### LangChain (`examples/langchain_tools.py`)

```python
from langchain_core.tools import Tool
from schemadex import SchemaCache
from examples.langchain_tools import make_schema_tools

cache = SchemaCache.from_url("sqlite:///demo.sqlite")
tools = make_schema_tools(cache)
# `tools` is a list of LangChain Tool objects: list_tables,
# describe_for_agent, resolve_column. Hand them to AgentExecutor.
```

### LangGraph (`examples/langgraph_node.py`)

```python
from langgraph.graph import StateGraph
from examples.langgraph_node import schema_node, AgentState

graph = StateGraph(AgentState)
graph.add_node("schema", schema_node(cache, max_tokens=2048))
graph.set_entry_point("schema")
```

### LlamaIndex (`examples/llamaindex_retriever.py`)

```python
from llama_index.core.query_engine import RetrieverQueryEngine
from examples.llamaindex_retriever import SchemaIndexRetriever

retriever = SchemaIndexRetriever(cache, max_tokens=2048)
engine = RetrieverQueryEngine.from_args(retriever)
response = engine.query("which regions had the most refunds?")
```

### DSPy (`examples/dspy_module.py`)

```python
import dspy
from examples.dspy_module import SchemadexContext

ctx = SchemadexContext(cache, max_tokens=2048)
prediction = ctx(question="orders by region")
print(prediction.schema)         # the schema description
print(prediction.schema_tokens)  # the token count
```

### LiteLLM (`examples/litellm_adapter.py`)

```python
from examples.litellm_adapter import schemadex_completion

resp = schemadex_completion(
    cache,
    "which regions had the most refunds?",
    model="ollama/qwen2.5-coder:3b",
)
print(resp.choices[0].message.content)
```

## 13. dbt manifest

When a project already has a dbt `manifest.json`, you can build a
SchemaCache directly off it — no live warehouse needed. Useful for
offline development, CI, and sandboxed environments where the agent
can't reach production:

```python
from schemadex import dbt_source

cache = dbt_source.from_manifest("target/manifest.json")
print(cache.list_tables())
```

Under the hood the manifest is projected into a synthetic SQLite
database whose schema mirrors every dbt model / source / seed. From
there it's the same `SchemaCache` you'd get from a live connection —
the same `resolve`, `describe_for_agent`, and so on.

## 14. Jupyter magic

A small IPython extension is bundled. Load it once, set a default URL,
then hit the cache from a notebook line:

```python
%load_ext schemadex

# Set a default URL for the rest of the session.
%schemadex_url sqlite:///tmp/demo.sqlite

# List every table.
%schemadex list-tables

# Describe one table.
%schemadex describe orders

# Resolve a column.
%schemadex resolve orders customer_idd
```

You can also pass `--url` inline if you switch databases mid-session:

```python
%schemadex --url postgres://app:secret@db/orders list-tables
```

## 15. Tracing + OpenTelemetry

`schemadex-core` emits structured `tracing` spans on every public entry
point. For Python users, the spans surface anywhere a Rust
`tracing_subscriber` is installed.

Plain log output via `RUST_LOG`:

```bash
# High-signal cache events; sqlx warnings only.
RUST_LOG=schemadex=info,sqlx=warn cargo run --example my_app

# Per-backend introspection calls.
RUST_LOG=schemadex_core::backends::postgres=debug,schemadex=info cargo run
```

For Rust users who want the same spans as OTLP traces, build with the
`otel` feature and call `init_otel` once at startup:

```rust
use schemadex_core::init_otel;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_otel("schemadex", "http://localhost:4317")?;
    // ... build SchemaCache, run queries — every span is now an OTLP span.
    Ok(())
}
```

The full collector recipe (Jaeger / Honeycomb / Datadog all work over
OTLP/gRPC on `4317`) lives in [observability.md](observability.md).

## 16. Schema diff

`schemadex-diff` is a small Rust binary that compares two cache
snapshots and prints a markdown changelog:

```bash
schemadex-diff --from cache_old.json --to cache_new.json
```

Each input is one of:

- a schemadex cache envelope (the JSON inside
  `~/.cache/schemadex/<url-hash>/database.json.zst` after decompression),
- or a bare `Database` JSON object emitted by `cache.to_json()`.

The on-disk cache is zstd-compressed; decompress it first or write a
fresh dump via `to_json()`:

```bash
zstd -d ~/.cache/schemadex/9b21c4e8…/database.json.zst -o old.json
# ... later, after a refresh:
zstd -d ~/.cache/schemadex/9b21c4e8…/database.json.zst -o new.json
schemadex-diff --from old.json --to new.json
```

Output covers tables added or removed, columns added or removed per
surviving table, and column-type changes. If both snapshots are
equivalent the binary prints `no changes` and exits 0.

## 17. Troubleshooting

**Q:** `ImportError: No module named 'schemadex._native'`
**A:** The compiled extension didn't ship with the wheel for your
platform. Reinstall via `pip install --force-reinstall schemadex`, or
build from source with `maturin develop --release` from a clone of the
repo.

**Q:** The cache returns stale data after a DDL change.
**A:** Call `cache.refresh(url)` (the DDL fingerprint check then leaves
unchanged tables alone), or delete `~/.cache/schemadex/<url-hash>/` to
force a full re-introspection on the next `from_url`.

**Q:** `resolve_with_embedding` always returns the lexical result.
**A:** Ollama isn't reachable or the model isn't pulled. Check that
`ollama serve` is running and try `curl
http://localhost:11434/api/tags` to confirm. The fallback is silent on
purpose — it logs to stderr and never raises so it can't break an
agent loop.

**Q:** MSSQL fails with `Login failed` against a self-signed cert.
**A:** Append `?encrypt=true&trust_cert=true` to the URL — without
`trust_cert=true`, TDS rejects the certificate before authentication.

**Q:** BigQuery raises `ApplicationDefaultCredentialsError`.
**A:** Run `gcloud auth application-default login` once on the host, or
set `GOOGLE_APPLICATION_CREDENTIALS` to a service-account JSON path.

**Q:** `pip install schemadex` fails on ARM Linux.
**A:** Before v1.0.0 there was no manylinux-aarch64 wheel; pip would
fall back to building from source, which needs a Rust toolchain. Either
install a recent rustc and let it build, or upgrade to v1.0.0+ where
the wheel is published.

**Q:** `run_sql` hangs on the first call.
**A:** That's the connection-pool handshake. Subsequent calls reuse
the pool. If it never returns, the URL is wrong or the database is
unreachable; cancel the call and verify with a plain `psql` /
`mysql` / etc. client.

**Q:** `cache.to_json()` is huge.
**A:** Expected — it's the full uncompressed dump. The on-disk
`database.json.zst` is the same content zstd-compressed (~10× smaller
on real schemas).

## 18. Versioning and stability

`schemadex` follows [SemVer 2.0.0](https://semver.org/). The exact
contract — what counts as public, what counts as breaking — is spelled
out in [semver.md](semver.md). Pre-1.0 minor versions (`0.x.0`) may
break; patch versions (`0.x.y`) won't.

For 0.x → 1.0 migration notes see
[migration-0.x-to-1.0.md](migration-0.x-to-1.0.md). The deprecation
policy (one minor with `DeprecationWarning` before any public removal)
lives in [deprecation.md](deprecation.md).
