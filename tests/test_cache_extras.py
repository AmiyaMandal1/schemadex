"""Smoke tests for the cache-side extras: embeddings, history snapshots,
result memoization, and CDC-style invalidation.

These tests exercise the Python bindings, not the underlying logic — the
real correctness checks live in the Rust unit tests next to the
implementation."""

from __future__ import annotations

import sqlite3
from pathlib import Path

import pytest

schemadex = pytest.importorskip("schemadex")


def _seed(path: Path) -> None:
    conn = sqlite3.connect(path)
    conn.executescript(
        """
        CREATE TABLE users (
            id INTEGER PRIMARY KEY,
            email TEXT NOT NULL,
            status TEXT
        );
        INSERT INTO users (email, status) VALUES
            ('a@x.com', 'active'),
            ('b@x.com', 'inactive');
        """
    )
    conn.commit()
    conn.close()


@pytest.fixture()
def cache(tmp_path: Path):
    db = tmp_path / "demo.sqlite"
    _seed(db)
    return schemadex.SchemaCache.from_url(
        f"sqlite://{db}",
        cache_dir=str(tmp_path / "cache"),
        history=3,
        memoize_results=True,
        memo_capacity=8,
    )


def test_store_and_load_embeddings(cache) -> None:
    """``store_embeddings`` round-trips through ``load_embeddings``."""
    payload = {
        "model": "nomic-embed-text-v2-moe",
        "dim": 3,
        "by_column": {
            "users": {
                "email": [0.1, 0.2, 0.3],
                "status": [0.4, 0.5, 0.6],
            }
        },
    }
    cache.store_embeddings(payload)
    loaded = cache.load_embeddings()
    assert loaded is not None
    assert loaded["model"] == payload["model"]
    assert loaded["dim"] == payload["dim"]
    cols = loaded["by_column"]["users"]
    assert "email" in cols
    assert len(cols["email"]) == 3


def test_invalidate_table_does_not_raise(cache) -> None:
    """``invalidate_table`` is the CDC hook — it must accept a known table
    name and return without error."""
    cache.invalidate_table("users")
    # Idempotent — calling twice is fine.
    cache.invalidate_table("users")


def test_invalidate_unknown_table_raises(cache) -> None:
    with pytest.raises(RuntimeError):
        cache.invalidate_table("does_not_exist")


def test_history_starts_with_initial_snapshot(cache) -> None:
    """``history()`` returns the snapshots written during populate."""
    hist = cache.history()
    assert isinstance(hist, list)
    # One snapshot was auto-written when the cache was first populated.
    assert len(hist) >= 1
    ts, tables = hist[-1]
    assert isinstance(ts, int)
    assert "users" in tables


def test_manual_snapshot_returns_path(cache, tmp_path: Path) -> None:
    """``snapshot()`` returns the path of the newly-written file."""
    path = cache.snapshot()
    assert isinstance(path, str)
    assert path.endswith(".json.zst")
    assert Path(path).exists()
