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

## Benchmarks

**Synthetic adversarial corpus** (`benches/agent-success/`): 38 column-name hallucinations modeled after real LLM SQL agent failures — case flips, missing/added underscores, plural/singular drift, semantic near-misses.

Two harnesses, same corpus:

| Harness          | Baseline | Treatment | Δ |
|------------------|---------:|----------:|---:|
| Literal lookup *(no LLM)*                |    0.0% |    94.7% | **+94.7 pp** |
| `qwen2.5-coder:3b` via Ollama *(real LLM)* |    97.4% |    100.0% | **+2.6 pp** |

Same model in both halves of the LLM row — the difference is `describe_for_agent` ranking + `resolve_column` post-correction. The single miss in the LLM baseline is `"What is the orderid on shipments?"` — the model picked `id` instead of `order_id`; resolve_column fixed it.

Median per-record latency: literal harness ~7 µs / 3 µs (schemadex overhead is in the noise); LLM harness ~285 ms (LLM-bound on an M2 Max).

Reproduce:

```bash
# Literal (no LLM)
python benches/agent-success/synthetic_corpus.py
python benches/agent-success/run_synthetic.py

# Real LLM via Ollama
ollama serve &
ollama pull qwen2.5-coder:3b
python benches/agent-success/run_ollama.py --model qwen2.5-coder:3b --n 0
```

This is a *micro-benchmark of the resolution path*, not an end-to-end LLM agent comparison on BIRD/Spider — those harnesses are scaffolded in `baseline.py` / `treatment.py` but require an API key + corpus download. See [`docs/benchmark.md`](docs/benchmark.md) for methodology.

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
| DuckDB   | `duckdb_backend`   | ✅     |
| MySQL    | —                  | planned (M8+) |
| BigQuery | —                  | planned (M8+) |
| Snowflake | —                 | planned (M8+) |

## MCP server

`schemadex` ships an MCP server. Install with `pip install "schemadex[mcp]"` and wire it into Claude Code by adding to `~/.claude/mcp.json`:

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

The agent then has `list_tables`, `describe_for_agent`, `resolve_column`, and `run_sql` tools without any extra wiring.

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
