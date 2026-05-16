"""Token-budget stress harness (v0.3 item 2).

Re-runs ablation cells C (schemadex dump, no post-correct) and D
(schemadex dump + post-correct) at three ``max_tokens`` floors: 256, 512,
1024. The point is to verify that ``describe_for_agent``'s truncation
hierarchy degrades gracefully: success_rate should plateau (or only drop
mildly) as the budget tightens, because the most-relevant tables are
kept and unrelated ones are dropped first.

For each budget we report ``(n, success_rate, median_latency_ms,
tokens_actually_used)`` where ``tokens_actually_used`` is the median of
the (token-count) values returned by ``describe_for_agent`` across the
questions in that run.

Outputs per-floor JSON into ``out/budget_<n>.json``. CLI flags mirror
``run_ablation.py``.

Run:
    python benches/agent-success/synthetic_corpus.py
    python benches/agent-success/run_token_budget.py --model qwen2.5-coder:3b --n 0
"""

from __future__ import annotations

import argparse
import json
import pathlib
import re
import statistics
import sys
import tempfile
import time
import urllib.request
from typing import Any

from schemadex import SchemaCache

HERE = pathlib.Path(__file__).parent
OUT = HERE / "out"
OLLAMA_URL = "http://localhost:11434/api/generate"

BUDGETS = (256, 512, 1024)


# ---------------------------------------------------------------------------
# Helpers (copied to keep the harness standalone).
# ---------------------------------------------------------------------------


def call_ollama(model: str, prompt: str, timeout: float = 60.0) -> str:
    body = json.dumps({"model": model, "prompt": prompt, "stream": False}).encode()
    req = urllib.request.Request(
        OLLAMA_URL, data=body, headers={"Content-Type": "application/json"}
    )
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        payload = json.load(resp)
    return payload.get("response", "")


def seed_db(db_path: pathlib.Path, schema_sql_path: pathlib.Path) -> None:
    import sqlite3

    conn = sqlite3.connect(db_path)
    conn.executescript(schema_sql_path.read_text())
    conn.commit()
    conn.close()


JSON_RE = re.compile(r"\{[^{}]*\}")


def extract_json(text: str) -> dict[str, Any] | None:
    cleaned = text.strip()
    if cleaned.startswith("```"):
        cleaned = re.sub(r"^```[a-zA-Z0-9]*\n?", "", cleaned)
        cleaned = re.sub(r"\n?```$", "", cleaned)
    for match in JSON_RE.finditer(cleaned):
        try:
            return json.loads(match.group(0))
        except json.JSONDecodeError:
            continue
    try:
        return json.loads(cleaned)
    except json.JSONDecodeError:
        return None


def make_prompt(schema_dump: str, question: str) -> str:
    return (
        "You are a SQL agent. Given the schema description below and a "
        "question, respond with ONLY a JSON object of the form "
        '{"table": "<table_name>", "column": "<column_name>"} '
        "naming the single column that answers the question.\n\n"
        "Schema:\n"
        f"{schema_dump}\n\n"
        f"Question: {question}\n\n"
        "JSON:"
    )


# ---------------------------------------------------------------------------
# Per-budget runner.
# ---------------------------------------------------------------------------


def evaluate_budget(
    budget: int,
    corpus: list[dict[str, str]],
    cache: SchemaCache,
    model: str,
) -> dict[str, Any]:
    """Run cells C and D at one ``max_tokens`` floor."""
    cell_results: dict[str, dict[str, Any]] = {}
    tokens_used: list[int] = []

    for cell in ("C", "D"):
        successes = 0
        latencies_ms: list[float] = []
        raw_log: list[dict[str, Any]] = []
        post_correct = cell == "D"

        for rec in corpus:
            question = rec["question"]
            try:
                dump, tok = cache.describe_for_agent(
                    max_tokens=budget, hint=question, include_samples=False
                )
                tokens_used.append(int(tok))
            except Exception as exc:  # noqa: BLE001 — describe may raise on tiny budgets
                dump = f"(describe_for_agent failed at budget={budget}: {exc})"
            prompt = make_prompt(dump, question)

            t0 = time.perf_counter()
            try:
                response = call_ollama(model, prompt)
            except Exception as exc:  # noqa: BLE001
                response = f"ERROR: {exc}"
            latencies_ms.append((time.perf_counter() - t0) * 1000.0)

            parsed = extract_json(response) or {}
            candidate_col = str(parsed.get("column", "")).strip()
            chosen = candidate_col

            if post_correct and candidate_col:
                try:
                    r = cache.resolve(rec["table"], candidate_col)
                    if r.matched and r.confidence >= 0.85:
                        chosen = r.matched
                except Exception:  # noqa: BLE001
                    pass

            ok = chosen == rec["gold_column"]
            successes += int(ok)
            raw_log.append(
                {
                    "question": question,
                    "gold": rec["gold_column"],
                    "chosen": chosen,
                    "raw": response[:200],
                    "ok": ok,
                }
            )

        n = len(corpus)
        cell_results[cell] = {
            "n": n,
            "success_rate": successes / n if n else 0.0,
            "median_latency_ms": statistics.median(latencies_ms) if latencies_ms else 0.0,
            "raw": raw_log,
        }

    summary = {
        "budget": budget,
        "median_tokens_used": int(statistics.median(tokens_used)) if tokens_used else 0,
        "max_tokens_used": max(tokens_used) if tokens_used else 0,
        "cells": {
            cell: {k: v for k, v in cell_results[cell].items() if k != "raw"}
            for cell in ("C", "D")
        },
    }
    OUT.mkdir(exist_ok=True)
    (OUT / f"budget_{budget}.json").write_text(json.dumps(summary, indent=2))
    (OUT / f"budget_{budget}_raw.json").write_text(
        json.dumps(
            {cell: cell_results[cell]["raw"] for cell in ("C", "D")},
            indent=2,
        )
    )
    return summary


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--model", default="qwen2.5-coder:3b")
    ap.add_argument("--n", type=int, default=0, help="0 = full corpus")
    args = ap.parse_args(argv)

    corpus_path = HERE / "synthetic_corpus.json"
    schema_path = HERE / "synthetic_schema.sql"
    if not corpus_path.exists() or not schema_path.exists():
        print("missing inputs — run synthetic_corpus.py first", file=sys.stderr)
        return 1

    corpus = json.loads(corpus_path.read_text())
    if args.n > 0:
        corpus = corpus[: args.n]

    with tempfile.TemporaryDirectory() as tmp:
        db = pathlib.Path(tmp) / "bench.sqlite"
        seed_db(db, schema_path)
        cache = SchemaCache.from_url(
            f"sqlite://{db}", cache_dir=str(pathlib.Path(tmp) / "cache")
        )

        print(f"model={args.model}  n={len(corpus)}")
        results: list[dict[str, Any]] = []
        for budget in BUDGETS:
            print(f"running budget={budget}...")
            results.append(evaluate_budget(budget, corpus, cache, args.model))

    header = (
        f"\n{'budget':>7} {'n':>4} {'C_success':>10} {'D_success':>10} "
        f"{'C_med_lat':>10} {'D_med_lat':>10} {'tok_used':>9}"
    )
    print(header)
    for r in results:
        c = r["cells"]["C"]
        d = r["cells"]["D"]
        print(
            f"{r['budget']:>7} {c['n']:>4} "
            f"{c['success_rate']:>10.3f} {d['success_rate']:>10.3f} "
            f"{c['median_latency_ms']:>10.1f} {d['median_latency_ms']:>10.1f} "
            f"{r['median_tokens_used']:>9}"
        )
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
