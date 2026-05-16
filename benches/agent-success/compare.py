"""Aggregate baseline + treatment JSONL into a single numbers table."""

from __future__ import annotations

import json
import pathlib
import statistics
from typing import Any


def load(p: pathlib.Path) -> list[dict[str, Any]]:
    return [json.loads(line) for line in p.read_text().splitlines() if line.strip()]


def summary(rows: list[dict[str, Any]]) -> dict[str, float]:
    if not rows:
        return {"n": 0, "success_rate": 0.0, "median_retries": 0.0, "median_latency_ms": 0.0}
    return {
        "n": len(rows),
        "success_rate": sum(r["ok"] for r in rows) / len(rows),
        "median_retries": statistics.median(r["retries"] for r in rows),
        "median_latency_ms": statistics.median(r["latency_ms"] for r in rows),
    }


def main(out_dir: str = "out") -> None:
    base = summary(load(pathlib.Path(out_dir) / "baseline.jsonl"))
    treat = summary(load(pathlib.Path(out_dir) / "treatment.jsonl"))
    print(f"{'metric':<24} {'baseline':>10} {'treatment':>10}")
    for k in ("success_rate", "median_retries", "median_latency_ms"):
        print(f"{k:<24} {base[k]:>10.3f} {treat[k]:>10.3f}")


if __name__ == "__main__":
    main()
