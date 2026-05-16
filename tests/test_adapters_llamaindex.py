"""Tests for the LlamaIndex retriever adapter in ``examples/llamaindex_retriever.py``.

Skips cleanly if ``llama_index`` isn't installed in the active environment.
"""

from __future__ import annotations

import sqlite3
import sys
from pathlib import Path

import pytest

pytest.importorskip("llama_index.core.retrievers")

# The adapter lives under ``examples/``, which isn't on ``sys.path`` by default.
EXAMPLES_DIR = Path(__file__).resolve().parent.parent / "examples"
if str(EXAMPLES_DIR) not in sys.path:
    sys.path.insert(0, str(EXAMPLES_DIR))

import schemadex  # noqa: E402
from llama_index.core.schema import QueryBundle  # noqa: E402

from llamaindex_retriever import SchemaIndexRetriever  # noqa: E402


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
            ('EU', 7.5),
            ('APAC', 19.0);
        """
    )
    conn.commit()
    conn.close()


def test_retriever_returns_single_schema_node(tmp_path: Path) -> None:
    db = tmp_path / "demo.sqlite"
    _seed(db)
    cache = schemadex.SchemaCache.from_url(
        f"sqlite://{db}", cache_dir=str(tmp_path / "cache")
    )

    retriever = SchemaIndexRetriever(cache, max_tokens=1024)
    nodes = retriever._retrieve(QueryBundle(query_str="orders by region"))

    assert len(nodes) == 1
    text = nodes[0].node.get_content()
    assert "orders" in text
    assert nodes[0].node.metadata.get("schema_tokens", 0) > 0
