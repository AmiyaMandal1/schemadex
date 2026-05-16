"""4-cell ablation harness for the agent-success bench (v0.3 item 1).

Measures the independent contribution of (a) the schemadex schema dump and
(b) the post-LLM ``resolve_column`` correction step. Four cells:

    | Cell | schema dump                | post-correct       |
    |------|----------------------------|--------------------|
    | A    | raw `information_schema`   | none               |
    | B    | raw `information_schema`   | resolve_column     |
    | C    | `describe_for_agent`       | none               |
    | D    | `describe_for_agent`       | resolve_column     |

Every cell sees the same model, same questions, same gold answers. The
delta A->C isolates the dump contribution; B->D isolates resolve under a
schemadex dump; A->B isolates resolve under a raw dump. C/D vs A/B
together quantify the joint effect.

Outputs per-cell JSON into ``out/ablation_<cell>.json`` and a per-record
raw log into ``out/ablation_<cell>_raw.json``. Prints a 4-row summary
table at the end.

Run:
    ollama serve &
    ollama pull qwen2.5-coder:3b
    python benches/agent-success/synthetic_corpus.py
    python benches/agent-success/run_ablation.py --model qwen2.5-coder:3b --n 0
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


# ---------------------------------------------------------------------------
# Helpers (copied from run_ollama.py to keep this harness standalone).
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
    # Strip common code fences first.
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


def raw_schema_dump(cache: SchemaCache) -> str:
    """Mimic an information_schema-style dump: every column, no ranking, no samples."""
    parts: list[str] = []
    for table_name in cache.list_tables():
        tbl = cache.get_table(table_name) or {}
        cols = ", ".join(c["name"] for c in tbl.get("columns", []))
        parts.append(f"{table_name}({cols})")
    return "\n".join(parts)


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
# Cell runner.
# ---------------------------------------------------------------------------


CELLS: dict[str, dict[str, bool]] = {
    "A": {"schemadex_dump": False, "post_correct": False},
    "B": {"schemadex_dump": False, "post_correct": True},
    "C": {"schemadex_dump": True, "post_correct": False},
    "D": {"schemadex_dump": True, "post_correct": True},
}


def evaluate_cell(
    cell: str,
    corpus: list[dict[str, str]],
    cache: SchemaCache,
    model: str,
) -> dict[str, Any]:
    cfg = CELLS[cell]
    raw_dump = raw_schema_dump(cache)

    successes = 0
    retries: list[int] = []
    latencies_ms: list[float] = []
    raw_log: list[dict[str, Any]] = []

    for rec in corpus:
        question = rec["question"]
        if cfg["schemadex_dump"]:
            dump, _ = cache.describe_for_agent(
                max_tokens=1024, hint=question, include_samples=False
            )
        else:
            dump = raw_dump
        prompt = make_prompt(dump, question)

        t0 = time.perf_counter()
        try:
            response = call_ollama(model, prompt)
        except Exception as exc:  # noqa: BLE001 — bench wants to keep going
            response = f"ERROR: {exc}"
        latencies_ms.append((time.perf_counter() - t0) * 1000.0)

        parsed = extract_json(response) or {}
        candidate_col = str(parsed.get("column", "")).strip()
        chosen = candidate_col

        if cfg["post_correct"] and candidate_col:
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
        "cell": cell,
        "schemadex_dump": cfg["schemadex_dump"],
        "post_correct": cfg["post_correct"],
        "n": n,
        "success_rate": successes / n if n else 0.0,
        "median_retries": statistics.median(retries) if retries else 0.0,
        "median_latency_ms": statistics.median(latencies_ms) if latencies_ms else 0.0,
        "p95_latency_ms": (
            statistics.quantiles(latencies_ms, n=20)[18] if len(latencies_ms) >= 20 else 0.0
        ),
    }
    OUT.mkdir(exist_ok=True)
    (OUT / f"ablation_{cell}.json").write_text(json.dumps(summary, indent=2))
    (OUT / f"ablation_{cell}_raw.json").write_text(json.dumps(raw_log, indent=2))
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
        results: dict[str, dict[str, Any]] = {}
        for cell in ("A", "B", "C", "D"):
            print(f"running cell {cell}...")
            results[cell] = evaluate_cell(cell, corpus, cache, args.model)

    header = (
        f"\n{'cell':<5} {'dump':<10} {'correct':<8} {'n':>4} "
        f"{'success':>9} {'med_lat_ms':>11} {'p95_ms':>9}"
    )
    print(header)
    for cell in ("A", "B", "C", "D"):
        r = results[cell]
        dump_label = "schemadex" if r["schemadex_dump"] else "raw"
        correct_label = "yes" if r["post_correct"] else "no"
        print(
            f"{cell:<5} {dump_label:<10} {correct_label:<8} {r['n']:>4} "
            f"{r['success_rate']:>9.3f} {r['median_latency_ms']:>11.1f} "
            f"{r['p95_latency_ms']:>9.1f}"
        )
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
