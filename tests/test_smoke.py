"""End-to-end smoke test against SQLite. Verifies the native module is
importable and the high-level API works.

Run with `pytest tests/`.
"""

from __future__ import annotations

import sqlite3
from pathlib import Path

import pytest

schemadex = pytest.importorskip("schemadex")


def seed(path: Path) -> None:
    conn = sqlite3.connect(path)
    conn.executescript(
        """
        CREATE TABLE users (
            id INTEGER PRIMARY KEY,
            email TEXT NOT NULL,
            delay_code TEXT
        );
        INSERT INTO users (email, delay_code) VALUES
            ('a@x.com', 'No Delay'),
            ('b@x.com', 'No Delay'),
            ('c@x.com', 'No Delay'),
            ('d@x.com', 'No Delay'),
            ('e@x.com', 'Backhaul');
        """
    )
    conn.commit()
    conn.close()


def test_list_and_resolve(tmp_path: Path) -> None:
    db = tmp_path / "demo.sqlite"
    seed(db)
    cache = schemadex.SchemaCache.from_url(
        f"sqlite://{db}", cache_dir=str(tmp_path / "cache")
    )
    tables = cache.list_tables()
    assert "users" in tables

    r = cache.resolve("users", "delaycode")
    assert r.matched == "delay_code"
    assert r.confidence > 0.9


def test_describe_returns_tokens(tmp_path: Path) -> None:
    db = tmp_path / "demo.sqlite"
    seed(db)
    cache = schemadex.SchemaCache.from_url(
        f"sqlite://{db}", cache_dir=str(tmp_path / "cache")
    )
    prompt, tokens = cache.describe_for_agent(max_tokens=1024)
    assert "users" in prompt
    assert 0 < tokens < 1024


def test_sample_values_flag_runs(tmp_path: Path) -> None:
    """Passing ``sample_values=True`` must not raise and the resulting cache
    must still parse the schema correctly.

    The seed data deliberately gives ``delay_code`` an 80% concentration of
    ``'No Delay'`` (4 of 5 rows) — matching the Nokia sentinel story. Once
    sqlite gains sample-value support we'll extend this test to assert that
    the sentinel actually fires. For now the sqlite backend silently ignores
    the sampling policy, so we only check the wiring doesn't blow up.
    """
    db = tmp_path / "demo.sqlite"
    seed(db)
    cache = schemadex.SchemaCache.from_url(
        f"sqlite://{db}",
        cache_dir=str(tmp_path / "cache"),
        sample_values=True,
        sample_top_k=5,
        sample_sentinel_threshold=0.4,
        sample_rows=1000,
    )
    assert "users" in cache.list_tables()
    table = cache.get_table("users")
    assert table is not None
    column_names = {c["name"] for c in table["columns"]}
    assert {"id", "email", "delay_code"}.issubset(column_names)
