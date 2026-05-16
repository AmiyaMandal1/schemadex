"""Treatment harness — agent + schemadex."""

from __future__ import annotations

import json
import pathlib
from typing import Any

from schemadex import SchemaCache

OUT = pathlib.Path(__file__).parent / "out"


def run(corpus_path: str, db_url: str, model: str = "gpt-4o-mini") -> dict[str, Any]:
    OUT.mkdir(exist_ok=True)
    cache = SchemaCache.from_url(db_url)
    results: list[dict[str, Any]] = []
    corpus = json.loads(pathlib.Path(corpus_path).read_text())
    for item in corpus:
        prompt, tokens = cache.describe_for_agent(max_tokens=2048, hint=item["question"])
        # TODO: dispatch to model with prompt + schemadex resolve_column tool
        results.append(
            {
                "id": item["id"],
                "ok": False,
                "retries": 0,
                "latency_ms": 0,
                "schema_tokens": tokens,
            }
        )
    out_path = OUT / "treatment.jsonl"
    out_path.write_text("\n".join(json.dumps(r) for r in results))
    return {"out": str(out_path), "n": len(results)}


if __name__ == "__main__":
    import sys

    print(run(sys.argv[1], sys.argv[2]))
