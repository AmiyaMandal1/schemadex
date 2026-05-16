"""Sample-value contribution harness (v0.3 item 3).

Measures whether a sentinel-tagged schema description lets the LLM
answer a value-distribution question that pure schema metadata can't.

Two cells:

- **baseline**: ``describe_for_agent`` with ``sample_values=False``
  (schema names + types only, no value previews).
- **treatment**: same description, but augmented with the synthetic
  sentinel marker ``[sentinel: No Delay=80%]`` on ``delay_code``.

Implementation note — sentinel injection path
---------------------------------------------
The Python ``SchemaCache`` exposes ``to_json`` but no ``from_json``
constructor, so we cannot reliably round-trip a mutated cache through
the Rust core. The SQLite backend also does not collect sample values
yet. We therefore take the spec's documented fallback: hand-build the
two prompt strings directly. The baseline string is exactly what
``describe_for_agent(include_samples=False)`` would emit on this
toy schema; the treatment string is the same description with a single
``[sentinel: No Delay=80%]`` annotation appended to the
``delay_code`` line. This isolates the *sentinel annotation* as the
only difference between the two cells.

We still build a real ``SchemaCache`` to (a) prove the import surface
is intact and (b) confirm the corpus seed matches what schemadex sees.

Run:
    python benches/agent-success/sentinel_corpus.py
    python benches/agent-success/run_sentinel.py --model qwen2.5-coder:3b
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


def call_ollama(model: str, prompt: str, timeout: float = 60.0) -> str:
    body = json.dumps({"model": model, "prompt": prompt, "stream": False}).encode()
    req = urllib.request.Request(
        OLLAMA_URL, data=body, headers={"Content-Type": "application/json"}
    )
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        payload = json.load(resp)
    return payload.get("response", "")


def seed_db(db_path: pathlib.Path, seed_sql_path: pathlib.Path) -> None:
    import sqlite3

    conn = sqlite3.connect(db_path)
    conn.executescript(seed_sql_path.read_text())
    conn.commit()
    conn.close()


# ---------------------------------------------------------------------------
# Lenient JSON extraction with answer-string fallback.
# ---------------------------------------------------------------------------


JSON_RE = re.compile(r"\{[^{}]*\}", re.DOTALL)


def extract_answer(text: str) -> str:
    """Try hard to pull an `answer` field out of the model output."""
    cleaned = text.strip()
    if cleaned.startswith("```"):
        cleaned = re.sub(r"^```[a-zA-Z0-9]*\n?", "", cleaned)
        cleaned = re.sub(r"\n?```$", "", cleaned)

    for match in JSON_RE.finditer(cleaned):
        try:
            obj = json.loads(match.group(0))
        except json.JSONDecodeError:
            continue
        if isinstance(obj, dict) and "answer" in obj:
            return str(obj["answer"]).strip()

    try:
        obj = json.loads(cleaned)
        if isinstance(obj, dict) and "answer" in obj:
            return str(obj["answer"]).strip()
    except json.JSONDecodeError:
        pass

    # Last resort: regex for `"answer": "..."` or `answer: foo`.
    m = re.search(r'["\']?answer["\']?\s*:\s*["\']?([^"\'\n,}]+)', cleaned, re.I)
    if m:
        return m.group(1).strip()
    return cleaned[:80].strip()


# ---------------------------------------------------------------------------
# Hand-built prompt fragments.
# ---------------------------------------------------------------------------


BASELINE_DESCRIPTION = (
    "# outages\n"
    "- id: INTEGER NOT NULL\n"
    "- region: TEXT NOT NULL\n"
    "- delay_code: TEXT NOT NULL\n"
)

TREATMENT_DESCRIPTION = (
    "# outages\n"
    "- id: INTEGER NOT NULL\n"
    "- region: TEXT NOT NULL\n"
    "- delay_code: TEXT NOT NULL [sentinel: No Delay=80%]\n"
)


def make_prompt(description: str, question: str) -> str:
    return (
        "You are a SQL agent answering questions about a database using "
        "only the schema description provided. Respond with ONLY a JSON "
        'object of the form {"answer": "<value>"} naming the single most '
        "likely answer. Do not add prose.\n\n"
        "Schema:\n"
        f"{description}\n\n"
        f"Question: {question}\n\n"
        "JSON:"
    )


# ---------------------------------------------------------------------------
# Cell runner.
# ---------------------------------------------------------------------------


def evaluate_cell(
    cell: str,
    description: str,
    corpus: list[dict[str, str]],
    model: str,
) -> dict[str, Any]:
    successes = 0
    latencies_ms: list[float] = []
    raw_log: list[dict[str, Any]] = []

    for rec in corpus:
        prompt = make_prompt(description, rec["question"])
        t0 = time.perf_counter()
        try:
            response = call_ollama(model, prompt)
        except Exception as exc:  # noqa: BLE001
            response = f"ERROR: {exc}"
        latencies_ms.append((time.perf_counter() - t0) * 1000.0)

        answer = extract_answer(response)
        gold = rec["gold_answer"].lower()
        ok = gold in answer.lower()
        successes += int(ok)
        raw_log.append(
            {
                "question": rec["question"],
                "gold": rec["gold_answer"],
                "answer": answer,
                "raw": response[:200],
                "ok": ok,
            }
        )

    n = len(corpus)
    summary = {
        "cell": cell,
        "n": n,
        "success_rate": successes / n if n else 0.0,
        "median_latency_ms": statistics.median(latencies_ms) if latencies_ms else 0.0,
    }
    OUT.mkdir(exist_ok=True)
    (OUT / f"sentinel_{cell}.json").write_text(json.dumps(summary, indent=2))
    (OUT / f"sentinel_{cell}_raw.json").write_text(json.dumps(raw_log, indent=2))
    return summary


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--model", default="qwen2.5-coder:3b")
    args = ap.parse_args(argv)

    corpus_path = HERE / "sentinel_corpus.json"
    seed_path = HERE / "sentinel_seed.sql"
    if not corpus_path.exists() or not seed_path.exists():
        print("missing inputs — run sentinel_corpus.py first", file=sys.stderr)
        return 1

    corpus = json.loads(corpus_path.read_text())

    # We still spin up SchemaCache to keep the import surface honest and
    # to print a sanity line — the actual prompts are hand-built above.
    with tempfile.TemporaryDirectory() as tmp:
        db = pathlib.Path(tmp) / "sentinel.sqlite"
        seed_db(db, seed_path)
        cache = SchemaCache.from_url(
            f"sqlite://{db}", cache_dir=str(pathlib.Path(tmp) / "cache")
        )
        tables = cache.list_tables()
        print(f"model={args.model}  tables={tables}  n={len(corpus)}")

        print("running baseline (no sample info)...")
        baseline = evaluate_cell("baseline", BASELINE_DESCRIPTION, corpus, args.model)
        print("running treatment (with sentinel)...")
        treatment = evaluate_cell("treatment", TREATMENT_DESCRIPTION, corpus, args.model)

    print(f"\n{'cell':<10} {'n':>3} {'success':>9} {'med_lat_ms':>11}")
    for r in (baseline, treatment):
        print(
            f"{r['cell']:<10} {r['n']:>3} "
            f"{r['success_rate']:>9.3f} {r['median_latency_ms']:>11.1f}"
        )
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
