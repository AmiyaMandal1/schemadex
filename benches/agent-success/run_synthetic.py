"""Run the synthetic baseline-vs-treatment bench.

Baseline:  treat the agent's candidate column name as authoritative; the
           query "succeeds" iff the candidate is a real column on the table.
Treatment: route the candidate through `SchemaCache.resolve`, threshold
           confidence at >=0.85, accept the resolved column as the query
           target.

Metrics:
- success_rate:   fraction of records where the chosen column == gold
- retry_count:    1 if the chosen column != gold, else 0 (proxy for LLM
                  retry cost — a hallucinated column triggers a retry loop
                  in a real agent)
- latency_ms:     wall time for the schema-side step (lookup vs. resolve)

Run:
    python benches/agent-success/synthetic_corpus.py   # generate inputs
    python benches/agent-success/run_synthetic.py
"""

from __future__ import annotations

import json
import pathlib
import sqlite3
import statistics
import sys
import tempfile
import time
from typing import Callable

from schemadex import SchemaCache

HERE = pathlib.Path(__file__).parent
OUT = HERE / "out"


def seed_db(db_path: pathlib.Path, schema_sql_path: pathlib.Path) -> None:
    conn = sqlite3.connect(db_path)
    conn.executescript(schema_sql_path.read_text())
    conn.commit()
    conn.close()


def baseline_lookup(cache: SchemaCache, table: str, candidate: str) -> str | None:
    """Treat candidate verbatim. Match iff it's a real column on the table."""
    row = cache.get_table(table) or {}
    cols = {c["name"] for c in row.get("columns", [])}
    return candidate if candidate in cols else None


def treatment_resolve(cache: SchemaCache, table: str, candidate: str) -> str | None:
    """Route through resolve_column with a confidence floor."""
    r = cache.resolve(table, candidate)
    if r.matched is None or r.confidence < 0.85:
        return None
    return r.matched


def evaluate(
    corpus: list[dict[str, str]],
    cache: SchemaCache,
    chooser: Callable[[SchemaCache, str, str], str | None],
) -> dict[str, float]:
    successes = 0
    retries: list[int] = []
    latencies_ms: list[float] = []
    for rec in corpus:
        t0 = time.perf_counter()
        chosen = chooser(cache, rec["table"], rec["agent_candidate"])
        latencies_ms.append((time.perf_counter() - t0) * 1000.0)
        ok = chosen == rec["gold_column"]
        successes += int(ok)
        retries.append(0 if ok else 1)
    n = len(corpus)
    return {
        "n": n,
        "success_rate": successes / n if n else 0.0,
        "median_retries": statistics.median(retries) if retries else 0.0,
        "median_latency_ms": statistics.median(latencies_ms) if latencies_ms else 0.0,
        "p95_latency_ms": (
            statistics.quantiles(latencies_ms, n=20)[18] if len(latencies_ms) >= 20 else 0.0
        ),
    }


def main() -> int:
    corpus_path = HERE / "synthetic_corpus.json"
    schema_path = HERE / "synthetic_schema.sql"
    if not corpus_path.exists() or not schema_path.exists():
        print("missing inputs — run synthetic_corpus.py first", file=sys.stderr)
        return 1

    corpus = json.loads(corpus_path.read_text())

    with tempfile.TemporaryDirectory() as tmp:
        db = pathlib.Path(tmp) / "bench.sqlite"
        seed_db(db, schema_path)
        cache_dir = pathlib.Path(tmp) / "cache"
        cache = SchemaCache.from_url(f"sqlite://{db}", cache_dir=str(cache_dir))

        baseline = evaluate(corpus, cache, baseline_lookup)
        treatment = evaluate(corpus, cache, treatment_resolve)

    OUT.mkdir(exist_ok=True)
    (OUT / "synthetic_baseline.json").write_text(json.dumps(baseline, indent=2))
    (OUT / "synthetic_treatment.json").write_text(json.dumps(treatment, indent=2))

    print(f"{'metric':<22} {'baseline':>10} {'treatment':>10}")
    for k in ("n", "success_rate", "median_retries", "median_latency_ms", "p95_latency_ms"):
        b = baseline[k]
        t = treatment[k]
        print(f"{k:<22} {b:>10.3f} {t:>10.3f}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
