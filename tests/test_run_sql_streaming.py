"""End-to-end test for ``SchemaCache.run_sql_streaming`` against SQLite.

The streaming runner consumes rows one-at-a-time and bails out as soon as
its cheap token estimate exceeds the budget. We seed a wide-ish 1k-row
table so the full result would massively overshoot the budget, then verify
the returned markdown is short and carries the truncation marker.
"""

from __future__ import annotations

import sqlite3
from pathlib import Path

import pytest

schemadex = pytest.importorskip("schemadex")


def _seed_big(path: Path, rows: int = 1000) -> None:
    conn = sqlite3.connect(path)
    conn.executescript(
        """
        CREATE TABLE big (
            id INTEGER PRIMARY KEY,
            payload TEXT NOT NULL
        );
        """
    )
    # `payload` is wide enough that 1000 rows blow past any reasonable
    # streaming budget by a healthy margin.
    conn.executemany(
        "INSERT INTO big (id, payload) VALUES (?, ?)",
        [(i, f"row-{i}-payload-padding-{'x' * 40}") for i in range(1, rows + 1)],
    )
    conn.commit()
    conn.close()


def test_run_sql_streaming_truncates_to_budget(tmp_path: Path) -> None:
    db = tmp_path / "big.sqlite"
    _seed_big(db)
    url = f"sqlite://{db}"

    cache = schemadex.SchemaCache.from_url(url, cache_dir=str(tmp_path / "cache"))

    rendered, tokens = cache.run_sql_streaming(
        url, "SELECT * FROM big", token_budget=500
    )

    # The cheap estimator may slightly overshoot before the renderer trims
    # to fit. Allow a margin: the renderer is the source of truth and
    # promises `tokens <= budget` on success.
    assert tokens <= 500
    assert tokens > 0
    # The truncation marker must be present — the full set of 1000 rows
    # can't possibly fit under a 500-token budget.
    assert "truncated" in rendered
    # And we must have rendered at least the header + dashes + one row.
    assert "payload" in rendered
    assert "|" in rendered


def test_run_sql_streaming_blocks_writes(tmp_path: Path) -> None:
    db = tmp_path / "big.sqlite"
    _seed_big(db, rows=10)
    url = f"sqlite://{db}"

    cache = schemadex.SchemaCache.from_url(url, cache_dir=str(tmp_path / "cache"))

    with pytest.raises(RuntimeError, match="read-only"):
        cache.run_sql_streaming(url, "DELETE FROM big", token_budget=500)
