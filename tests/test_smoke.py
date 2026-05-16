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


def test_refresh_runs_without_error(tmp_path: Path) -> None:
    """``refresh`` and ``refresh_table`` re-introspect the live database and
    rewrite the persisted cache. Because the seed schema is stable between
    calls, every table should land in the ``unchanged`` bucket — but we only
    assert the structural contract here so the test stays valid if a future
    backend stamps a fresh DDL hash on every read.
    """
    db = tmp_path / "demo.sqlite"
    seed(db)
    # Add a second table so refresh has more than one entry to report on.
    conn = sqlite3.connect(db)
    conn.executescript(
        "CREATE TABLE orders (id INTEGER PRIMARY KEY, user_id INTEGER NOT NULL);"
    )
    conn.commit()
    conn.close()

    url = f"sqlite://{db}"
    cache = schemadex.SchemaCache.from_url(url, cache_dir=str(tmp_path / "cache"))
    table_count = len(cache.list_tables())
    assert table_count >= 2

    result = cache.refresh(url)
    assert isinstance(result, tuple) and len(result) == 2
    changed, unchanged = result
    assert isinstance(changed, list)
    assert isinstance(unchanged, list)
    assert len(changed) + len(unchanged) == table_count

    one = cache.refresh_table(url, "users")
    assert isinstance(one, tuple) and len(one) == 2
    one_changed, one_unchanged = one
    assert isinstance(one_changed, list)
    assert isinstance(one_unchanged, list)
    assert len(one_changed) + len(one_unchanged) <= 1


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
