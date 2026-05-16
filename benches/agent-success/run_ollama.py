"""Real-LLM bench using an Ollama model.

Compares two prompt strategies against the same synthetic schema:

- baseline:  raw `information_schema`-style dump (every column, no
             relevance ranking, no sample values), and the agent must
             emit a JSON object {"table": ..., "column": ...}.
- treatment: schemadex `describe_for_agent(hint=question)` plus a
             schemadex `resolve` step that fuzzy-corrects the agent's
             column name before we score it.

Both strategies see the same model, the same questions, and the same
gold answers. Metrics: success_rate, retries, latency.

Run:
    ollama serve &
    ollama pull qwen2.5-coder:3b
    python benches/agent-success/synthetic_corpus.py
    python benches/agent-success/run_ollama.py --model qwen2.5-coder:3b --n 20

This is intentionally small (default n=20 of 38) so a 3B model finishes
in a minute or two. Pass --n 0 to run the full corpus.
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
from typing import Any

import urllib.request

from schemadex import SchemaCache

HERE = pathlib.Path(__file__).parent
OUT = HERE / "out"
OLLAMA_URL = "http://localhost:11434/api/generate"


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
    # Models often pad with prose; grab the first JSON object.
    for match in JSON_RE.finditer(text):
        try:
            return json.loads(match.group(0))
        except json.JSONDecodeError:
            continue
    try:
        return json.loads(text)
    except json.JSONDecodeError:
        return None


def baseline_prompt(schema_dump: str, question: str) -> str:
    return (
        "You are a SQL agent. Given the schema dump below and a question, "
        "respond with ONLY a JSON object of the form "
        '{"table": "<table_name>", "column": "<column_name>"} '
        "naming the single column that answers the question.\n\n"
        "Schema:\n"
        f"{schema_dump}\n\n"
        f"Question: {question}\n\n"
        "JSON:"
    )


def treatment_prompt(schemadex_dump: str, question: str) -> str:
    return (
        "You are a SQL agent. Given the schema description below and a "
        "question, respond with ONLY a JSON object of the form "
        '{"table": "<table_name>", "column": "<column_name>"} '
        "naming the single column that answers the question.\n\n"
        "Schema:\n"
        f"{schemadex_dump}\n\n"
        f"Question: {question}\n\n"
        "JSON:"
    )


def raw_schema_dump(cache: SchemaCache) -> str:
    parts = []
    for table_name in cache.list_tables():
        tbl = cache.get_table(table_name) or {}
        cols = ", ".join(c["name"] for c in tbl.get("columns", []))
        parts.append(f"{table_name}({cols})")
    return "\n".join(parts)


def evaluate(
    corpus: list[dict[str, str]],
    cache: SchemaCache,
    model: str,
    strategy: str,
) -> dict[str, float]:
    schema_dump = raw_schema_dump(cache)
    successes = 0
    retries: list[int] = []
    latencies_ms: list[float] = []
    raw_log: list[dict[str, Any]] = []

    for rec in corpus:
        question = rec["question"]
        if strategy == "baseline":
            prompt = baseline_prompt(schema_dump, question)
        else:
            sd_dump, _ = cache.describe_for_agent(
                max_tokens=1024, hint=question, include_samples=False
            )
            prompt = treatment_prompt(sd_dump, question)

        t0 = time.perf_counter()
        try:
            response = call_ollama(model, prompt)
        except Exception as exc:  # noqa: BLE001 — bench wants to keep going
            response = f"ERROR: {exc}"
        latencies_ms.append((time.perf_counter() - t0) * 1000.0)

        parsed = extract_json(response) or {}
        candidate_col = str(parsed.get("column", "")).strip()
        chosen = candidate_col

        if strategy == "treatment" and candidate_col:
            try:
                r = cache.resolve(rec["table"], candidate_col)
                if r.matched and r.confidence >= 0.85:
                    chosen = r.matched
            except Exception:  # noqa: BLE001
                pass

        ok = chosen == rec["gold_column"]
        successes += int(ok)
        retries.append(0 if ok else 1)
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
    summary = {
        "n": n,
        "success_rate": successes / n if n else 0.0,
        "median_retries": statistics.median(retries) if retries else 0.0,
        "median_latency_ms": statistics.median(latencies_ms) if latencies_ms else 0.0,
        "p95_latency_ms": (
            statistics.quantiles(latencies_ms, n=20)[18] if len(latencies_ms) >= 20 else 0.0
        ),
    }
    OUT.mkdir(exist_ok=True)
    (OUT / f"ollama_{strategy}.json").write_text(json.dumps(summary, indent=2))
    (OUT / f"ollama_{strategy}_raw.json").write_text(json.dumps(raw_log, indent=2))
    return summary


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--model", default="qwen2.5-coder:3b")
    ap.add_argument("--n", type=int, default=20, help="0 = full corpus")
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
        print("running baseline...")
        b = evaluate(corpus, cache, args.model, "baseline")
        print("running treatment...")
        t = evaluate(corpus, cache, args.model, "treatment")

    print(f"\n{'metric':<22} {'baseline':>10} {'treatment':>10}")
    for k in ("n", "success_rate", "median_retries", "median_latency_ms", "p95_latency_ms"):
        print(f"{k:<22} {b[k]:>10.3f} {t[k]:>10.3f}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
