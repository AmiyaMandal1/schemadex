"""Async variants of the schemadex Python API, exercised from inside an
``asyncio`` event loop.

These tests run with ``pytest-asyncio`` (declared in the ``dev`` optional-deps
group). They mirror the smoke tests but await the ``*_async`` free-functions
instead of calling the synchronous methods.
"""

from __future__ import annotations

import sqlite3
from pathlib import Path

import pytest

schemadex = pytest.importorskip("schemadex")
pytest.importorskip("pytest_asyncio")


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
            ('c@x.com', 'No Delay');
        """
    )
    conn.commit()
    conn.close()


@pytest.mark.asyncio
async def test_async_from_url_runs(tmp_path: Path) -> None:
    db = tmp_path / "demo.sqlite"
    seed(db)
    cache = await schemadex.from_url_async(
        f"sqlite://{db}", cache_dir=str(tmp_path / "cache")
    )
    tables = cache.list_tables()
    assert "users" in tables


@pytest.mark.asyncio
async def test_async_run_sql_executes(tmp_path: Path) -> None:
    db = tmp_path / "demo.sqlite"
    seed(db)
    url = f"sqlite://{db}"
    cache = await schemadex.from_url_async(url, cache_dir=str(tmp_path / "cache"))

    rendered, tokens = await schemadex.run_sql_async(
        cache, url, "SELECT id, email FROM users ORDER BY id", token_budget=200
    )
    # The rendered output should contain the SELECT'd columns and at least one
    # email payload from the seed data.
    assert "email" in rendered
    assert "@" in rendered
    assert 0 < tokens <= 200


@pytest.mark.asyncio
async def test_async_refresh_runs(tmp_path: Path) -> None:
    db = tmp_path / "demo.sqlite"
    seed(db)
    url = f"sqlite://{db}"
    cache = await schemadex.from_url_async(url, cache_dir=str(tmp_path / "cache"))
    table_count = len(cache.list_tables())
    assert table_count >= 1

    changed, unchanged = await schemadex.refresh_async(cache, url)
    assert isinstance(changed, list)
    assert isinstance(unchanged, list)
    assert len(changed) + len(unchanged) == table_count

    one_changed, one_unchanged = await schemadex.refresh_table_async(cache, url, "users")
    assert isinstance(one_changed, list)
    assert isinstance(one_unchanged, list)
    assert len(one_changed) + len(one_unchanged) <= 1
