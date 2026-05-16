# schemadex

> Schema introspection and resolution toolkit for SQL agents.

`schemadex` is a Rust core with Python bindings that turns a live database into something an LLM agent can actually consume: a token-budgeted, sample-aware, fuzzy-resolvable view of every table and column, cached on disk so the agent doesn't re-pay the introspection cost on every turn.

## Install

```bash
pip install schemadex                            # core + all backends
pip install "schemadex[langchain,langgraph]"     # with framework adapters
```

Rust (git, not crates.io):

```toml
schemadex-core = { git = "https://github.com/AmiyaMandal1/schemadex", features = ["postgres", "sqlite", "duckdb_backend"] }
```

## 10-line example

```python
from schemadex import SchemaCache

cache = SchemaCache.from_url("postgres://localhost/mydb")

for name in cache.list_tables():
    print(name)

result = cache.resolve("public.orders", "customer_idd")
print(result.matched, result.confidence)   # 'customer_id', 0.98

prompt, tokens = cache.describe_for_agent(max_tokens=1500, hint="orders by region")
```

## Why

LLM SQL agents fail in the same three ways over and over:

1. They hallucinate column names because they got the schema in a thousand-token blob and forgot half of it.
2. They retry the same broken query because they don't know `'No Delay'` is the sentinel value covering 80% of `delay_code`.
3. They re-introspect on every step because there's nowhere obvious to cache the schema.

`schemadex` fixes all three:

- **Resolution**: `resolve_column(table, candidate)` returns a confidence + alternatives instead of letting the agent guess.
- **Sampling**: `sample_values=True` collects top-K, percentiles, and flags any value over 40% frequency as a sentinel.
- **Cache**: introspect once, persist to `~/.cache/schemadex/<db>/`, refresh on DDL change. On a local 50-table SQLite, warm reads are ~47× cold (`cargo bench --bench cache_refresh`); on remote Postgres the ratio grows since cold is network-bound.

## Backends

| Backend  | Feature flag       | Status |
|----------|--------------------|--------|
| Postgres | `postgres`         | ✅     |
| SQLite   | `sqlite`           | ✅     |
| DuckDB   | `duckdb_backend`   | ✅ (PK/FK omitted) |
| MySQL    | —                  | planned (M8+) |
| BigQuery | —                  | planned (M8+) |
| Snowflake | —                 | planned (M8+) |

## Layout

```
schemadex/
├── crates/
│   ├── schemadex-core/    pure-Rust introspection + cache + resolve
│   └── schemadex-py/      PyO3 bindings (built as `schemadex._native`)
├── python/schemadex/      pure-Python public surface
├── examples/              langchain, langgraph, quickstart
├── benches/agent-success/ benchmark harness (see docs/benchmark.md)
└── docs/                  architecture + benchmark methodology
```

## Project status

Pre-1.0. API is still in motion. See [`road_map.md`](./road_map.md) for milestone tracking.

## License

Licensed under either of **MIT** or **Apache-2.0** at your option.
