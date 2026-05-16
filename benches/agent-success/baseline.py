"""Baseline harness — agent + plain `psycopg`.

Skeleton only; the actual agent loop is intentionally left as a stub so the
benchmark crate can be wired to whichever model API the operator has access to.
"""

from __future__ import annotations

import json
import pathlib
from typing import Any

OUT = pathlib.Path(__file__).parent / "out"


def run(corpus_path: str, model: str = "gpt-4o-mini") -> dict[str, Any]:
    OUT.mkdir(exist_ok=True)
    results: list[dict[str, Any]] = []
    corpus = json.loads(pathlib.Path(corpus_path).read_text())
    for item in corpus:
        # TODO: dispatch to model with item["question"] + raw information_schema dump
        results.append({"id": item["id"], "ok": False, "retries": 0, "latency_ms": 0})
    out_path = OUT / "baseline.jsonl"
    out_path.write_text("\n".join(json.dumps(r) for r in results))
    return {"out": str(out_path), "n": len(results)}


if __name__ == "__main__":
    import sys

    print(run(sys.argv[1]))
