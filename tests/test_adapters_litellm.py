"""Tests for the LiteLLM adapter in ``examples/litellm_adapter.py``.

Skips cleanly if ``litellm`` isn't installed in the active environment.
The test only exercises ``schemadex_messages`` so no LLM call is made.
"""

from __future__ import annotations

import sqlite3
import sys
from pathlib import Path

import pytest

pytest.importorskip("litellm")

EXAMPLES_DIR = Path(__file__).resolve().parent.parent / "examples"
if str(EXAMPLES_DIR) not in sys.path:
    sys.path.insert(0, str(EXAMPLES_DIR))

import schemadex  # noqa: E402

from litellm_adapter import schemadex_messages  # noqa: E402


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


def test_messages_have_system_with_schema_and_user_question(tmp_path: Path) -> None:
    db = tmp_path / "demo.sqlite"
    _seed(db)
    cache = schemadex.SchemaCache.from_url(
        f"sqlite://{db}", cache_dir=str(tmp_path / "cache")
    )

    question = "total amount by region"
    messages = schemadex_messages(cache, question, max_tokens=1024)

    assert len(messages) == 2
    assert messages[0]["role"] == "system"
    assert messages[1]["role"] == "user"
    assert "orders" in messages[0]["content"]
    assert messages[1]["content"] == question
