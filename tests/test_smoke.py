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
