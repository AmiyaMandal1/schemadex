# Benchmark methodology

This document captures the methodology used by `benches/agent-success`. It exists so the numbers in the README survive "how did you measure?" questions.

## Corpus

- **BIRD-mini** (recommended): 100 questions across 11 databases, available from the [BIRD project](https://bird-bench.github.io/). We use the dev split.
- **Spider-dev** (alternate): 1034 questions across 200 databases.

We do not redistribute the dataset; `benches/agent-success/load_corpus.py` is a fetcher.

## Agents

We compare two agents over the same model:

- **Baseline** — the agent receives one block of `information_schema` at startup. Reset each question.
- **Treatment** — the agent receives `describe_for_agent(max_tokens=2048, hint=question)` plus a `resolve_column` tool.

Both agents share:

- The same model and temperature.
- The same maximum tool-call budget (8).
- The same SQL executor that returns either rows or `psycopg` error strings.

## Metrics

| Metric | Definition |
|--------|------------|
| SQL success rate | Fraction of questions where the agent emits a query whose result set matches the gold answer. |
| Retry count | Number of `tool_call` cycles before final answer. |
| Schema-discovery latency | Wall clock from question received to first SQL emitted. |

## Running

```bash
cd benches/agent-success
python load_corpus.py
python baseline.py corpus.json
python treatment.py corpus.json $DB_URL
python compare.py
```

## Reporting policy

Whatever the numbers are, we publish them. If `schemadex` doesn't help, the README says so.

## Synthetic adversarial corpus

The full BIRD/Spider harness needs an LLM API key. To give honest numbers for the **resolution path** without one, we ship a synthetic corpus and a deterministic chooser.

- **Corpus:** `benches/agent-success/synthetic_corpus.json`, 38 records covering case flips (`Id`, `ID`), underscore drift (`firstname` vs `first_name`), plural drift (`regions`, `currencies`), and semantic near-misses (`state` for `status`, `product_name` for `name`).
- **Schema:** `benches/agent-success/synthetic_schema.sql`, five tables, ~30 columns.
- **Baseline chooser:** literal lookup — the candidate succeeds iff it is exactly a column on the table.
- **Treatment chooser:** `SchemaCache.resolve(table, candidate)`, accept if `confidence >= 0.85`.

Reproduce:

```bash
python benches/agent-success/synthetic_corpus.py
python benches/agent-success/run_synthetic.py
```

Output (committed under `benches/agent-success/out/synthetic_*.json`):

| Metric              | Baseline | Treatment |
|---------------------|---------:|----------:|
| n                   |       38 |        38 |
| success_rate        |    0.000 |     0.947 |
| median_retries      |    1.000 |     0.000 |
| median_latency_ms   |    0.007 |     0.003 |
| p95_latency_ms      |    0.012 |     0.012 |

### What this measures and doesn't

This isolates **the resolution-path contribution**. Two of the 38 records still miss because their confidence is below the 0.85 floor (`state` → `status`, `body` → `review_body` are semantic-only matches that Jaro-Winkler scores lower than the lexical near-misses). Lower the floor and treatment success climbs further, at the cost of false-positive risk on totally unrelated names.

It does **not** measure: end-to-end LLM SQL accuracy, token-budget contributions, sentinel-flag contributions, cache-hit contributions. Those need the live-LLM harness in `baseline.py` / `treatment.py`.
