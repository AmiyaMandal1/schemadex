"""Tests for the schemadex MCP server.

Skip the whole module if the optional ``mcp`` SDK isn't installed. When it is,
we construct the FastMCP server in-process against a seeded SQLite database
and invoke each tool's underlying callable directly (not over stdio) — that
gives us a tight unit test of the tool wiring without spinning up a subprocess.
"""

from __future__ import annotations

import sqlite3
from pathlib import Path

import pytest

pytest.importorskip("mcp")
schemadex = pytest.importorskip("schemadex")

from schemadex.mcp_server import build_server  # noqa: E402


def _seed(path: Path) -> None:
    conn = sqlite3.connect(path)
    conn.executescript(
        """
        CREATE TABLE customers (
            id INTEGER PRIMARY KEY,
            email TEXT NOT NULL,
            delay_code TEXT
        );
        INSERT INTO customers (email, delay_code) VALUES
            ('a@x.com', 'No Delay'),
            ('b@x.com', 'No Delay'),
            ('c@x.com', 'Backhaul');
        """
    )
    conn.commit()
    conn.close()


def _get_tool_fn(server, name: str):
    tool = server._tool_manager.get_tool(name)
    assert tool is not None, f"tool {name!r} not registered"
    return tool.fn


@pytest.fixture()
def server(tmp_path: Path):
    db = tmp_path / "demo.sqlite"
    _seed(db)
    url = f"sqlite://{db}"
    # Point the cache at an ephemeral dir so the test doesn't touch real cache.
    # build_server uses SchemaCache.from_url(url) directly; we can't inject a
    # cache_dir without changing the signature, but the cache is keyed by URL
    # and the tmp DB path is unique per test, so this is still isolated.
    return build_server(url), url


def test_list_tables_returns_list(server) -> None:
    srv, _ = server
    fn = _get_tool_fn(srv, "list_tables")
    result = fn()
    assert isinstance(result, list)
    assert "customers" in result


def test_describe_for_agent_returns_str_with_table(server) -> None:
    srv, _ = server
    fn = _get_tool_fn(srv, "describe_for_agent")
    text = fn()
    assert isinstance(text, str)
    assert "customers" in text

    # Hint + custom budget should also work.
    text2 = fn("customers", 512)
    assert isinstance(text2, str)
    assert "customers" in text2


def test_resolve_column_returns_expected_dict(server) -> None:
    srv, _ = server
    fn = _get_tool_fn(srv, "resolve_column")
    result = fn("customers", "delaycode")
    assert isinstance(result, dict)
    assert set(result.keys()) == {"matched", "confidence", "alternatives"}
    assert result["matched"] == "delay_code"
    assert isinstance(result["confidence"], float)
    assert isinstance(result["alternatives"], list)


def test_run_sql_returns_markdown(server) -> None:
    srv, _ = server
    fn = _get_tool_fn(srv, "run_sql")
    text = fn("SELECT id, email FROM customers ORDER BY id")
    assert isinstance(text, str)
    # Markdown tables use pipes as column separators.
    assert "|" in text
    # Result payload should be visible.
    assert "@" in text
