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
