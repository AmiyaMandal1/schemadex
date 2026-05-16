# agent-success bench

Public benchmark harness for measuring whether `schemadex` actually helps a SQL
agent on a public corpus (BIRD-mini or Spider-dev).

This directory ships the *harness*, not the data. Run `python load_corpus.py`
to download the corpus locally (license-permitting).

## Harnesses

- `baseline.py` — agent + plain `psycopg`. The agent gets a single dump of
  `information_schema` at the start and nothing else.
- `treatment.py` — agent + `schemadex`. The agent gets
  `describe_for_agent(max_tokens=2048, hint=question)` plus a `resolve_column`
  tool.

Both harnesses run on the same corpus and write JSONL results to `./out/`.
`compare.py` aggregates the two runs and prints a numbers table:

| Metric                  | Baseline | Treatment | Δ |
|-------------------------|----------|-----------|----|
| SQL success rate (%)    | ?        | ?         | ?  |
| Retry count per query   | ?        | ?         | ?  |
| Schema-discovery latency (ms) | ?  | ?         | ?  |

See `docs/benchmark.md` for methodology and replication steps.
