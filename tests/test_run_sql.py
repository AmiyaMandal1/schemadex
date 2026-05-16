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
