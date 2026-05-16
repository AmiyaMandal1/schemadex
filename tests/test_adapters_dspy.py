"""Tests for the DSPy adapter in ``examples/dspy_module.py``.

Skips cleanly if ``dspy`` isn't installed in the active environment.
"""

from __future__ import annotations

import sqlite3
import sys
from pathlib import Path

import pytest

pytest.importorskip("dspy")

EXAMPLES_DIR = Path(__file__).resolve().parent.parent / "examples"
if str(EXAMPLES_DIR) not in sys.path:
    sys.path.insert(0, str(EXAMPLES_DIR))

import schemadex  # noqa: E402

from dspy_module import SchemadexContext  # noqa: E402


def _seed(path: Path) -> None:
    conn = sqlite3.connect(path)
    conn.executescript(
        """
        CREATE TABLE orders (
            id INTEGER PRIMARY KEY,
            region TEXT NOT NULL,
            amount REAL NOT NULL
        );
        INSERT INTO orders (region, amount) VALUES
            ('NA', 12.0),
            ('EU', 7.5);
        """
    )
    conn.commit()
    conn.close()


def test_dspy_context_emits_schema_and_tokens(tmp_path: Path) -> None:
    db = tmp_path / "demo.sqlite"
    _seed(db)
    cache = schemadex.SchemaCache.from_url(
        f"sqlite://{db}", cache_dir=str(tmp_path / "cache")
    )

    ctx = SchemadexContext(cache, max_tokens=1024)
    result = ctx.forward("orders")

    assert "orders" in result.schema
    assert result.schema_tokens > 0
    assert result.question == "orders"
