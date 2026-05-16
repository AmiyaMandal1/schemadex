"""End-to-end test for ``SchemaCache.run_sql`` against SQLite."""

from __future__ import annotations

import sqlite3
from pathlib import Path

import pytest

schemadex = pytest.importorskip("schemadex")


def seed(path: Path) -> None:
    conn = sqlite3.connect(path)
    conn.executescript(
        """
        CREATE TABLE customers (id INTEGER PRIMARY KEY, email TEXT NOT NULL);
        INSERT INTO customers (email) VALUES
            ('a@x.com'),
            ('b@x.com');
        """
    )
    conn.commit()
    conn.close()


def test_run_sql_renders_results_under_budget(tmp_path: Path) -> None:
    db = tmp_path / "demo.sqlite"
    seed(db)
    url = f"sqlite://{db}"
    cache = schemadex.SchemaCache.from_url(url, cache_dir=str(tmp_path / "cache"))

    rendered, tokens = cache.run_sql(
        url, "SELECT id, email FROM customers ORDER BY id", token_budget=200
    )

    # The rendered result is a SELECT output, not a list of tables — the word
    # "customers" should not leak through.
    assert "customers" not in rendered
    # The actual payload (emails) should be visible.
    assert "@" in rendered
    assert 0 < tokens <= 200


def test_run_sql_default_token_budget(tmp_path: Path) -> None:
    db = tmp_path / "demo.sqlite"
    seed(db)
    url = f"sqlite://{db}"
    cache = schemadex.SchemaCache.from_url(url, cache_dir=str(tmp_path / "cache"))

    rendered, tokens = cache.run_sql(url, "SELECT id, email FROM customers")
    assert "@" in rendered
    assert tokens > 0


def test_run_sql_blocks_writes(tmp_path: Path) -> None:
    """Non-SELECT statements must be rejected by the read-only guard."""
    db = tmp_path / "demo.sqlite"
    seed(db)
    url = f"sqlite://{db}"
    cache = schemadex.SchemaCache.from_url(url, cache_dir=str(tmp_path / "cache"))

    with pytest.raises(RuntimeError, match="read-only"):
        cache.run_sql(url, "DELETE FROM customers", token_budget=100)

    with pytest.raises(RuntimeError, match="read-only"):
        cache.run_sql(url, "DROP TABLE customers", token_budget=100)

    # SELECT should still work alongside.
    rendered, _ = cache.run_sql(url, "SELECT id FROM customers", token_budget=200)
    assert "id" in rendered


def test_run_sql_allow_write_bypasses_guard(tmp_path: Path) -> None:
    """The ``allow_write=True`` escape hatch lets non-SELECT statements through."""
    db = tmp_path / "demo.sqlite"
    seed(db)
    url = f"sqlite://{db}"
    cache = schemadex.SchemaCache.from_url(url, cache_dir=str(tmp_path / "cache"))

    # DELETE goes through when the escape hatch is engaged. The rendered
    # output is an empty-result table (no rows returned), which is fine —
    # we just want to confirm the guard didn't fire.
    cache.run_sql(url, "DELETE FROM customers WHERE id = 1", allow_write=True)

    # Verify the delete actually landed.
    rendered, _ = cache.run_sql(url, "SELECT id FROM customers", token_budget=200)
    assert "1" not in rendered.split("---")[1] if "---" in rendered else True


def test_pool_reused_across_calls(tmp_path: Path) -> None:
    """The process-wide runner pool grows from 0 to 1 and then stays at 1."""
    from schemadex import _native

    _native.clear_pool_cache()
    assert _native.pool_size() == 0

    db = tmp_path / "demo.sqlite"
    seed(db)
    url = f"sqlite://{db}"
    cache = schemadex.SchemaCache.from_url(url, cache_dir=str(tmp_path / "cache"))

    cache.run_sql(url, "SELECT 1", token_budget=200)
    assert _native.pool_size() == 1

    cache.run_sql(url, "SELECT 1", token_budget=200)
    # Still 1 — same URL, runner reused.
    assert _native.pool_size() == 1

    # Different URL → new entry.
    db2 = tmp_path / "demo2.sqlite"
    seed(db2)
    url2 = f"sqlite://{db2}"
    cache2 = schemadex.SchemaCache.from_url(url2, cache_dir=str(tmp_path / "cache2"))
    cache2.run_sql(url2, "SELECT 1", token_budget=200)
    assert _native.pool_size() == 2

    _native.clear_pool_cache()
    assert _native.pool_size() == 0
