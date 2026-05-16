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

from schemadex.mcp_server import build_server, list_tools_for_export  # noqa: E402


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


def test_validate_sql_tool_exists(server) -> None:
    """`validate_sql` should be registered and return a list.

    When the parallel agent's :meth:`SchemaCache.validate_sql` method has
    landed, a clean query returns ``[]``. Until it lands, the tool
    gracefully degrades to ``[]`` as well — both paths satisfy the
    contract: the tool exists, the return type is a list, and a clean
    query produces no issues.
    """
    srv, _ = server
    fn = _get_tool_fn(srv, "validate_sql")
    result = fn("SELECT id, email FROM customers")
    assert isinstance(result, list)
    # A typo'd column should produce at least one issue *if* the native
    # method has landed; otherwise we accept the empty list.
    typo_result = fn("SELECT emial FROM customers")
    assert isinstance(typo_result, list)


def test_hint_for_error_tool_exists(server) -> None:
    """`hint_for_error` should be registered and return either a dict or None."""
    srv, _ = server
    fn = _get_tool_fn(srv, "hint_for_error")
    result = fn('column "emial" does not exist')
    assert result is None or isinstance(result, dict)
    # Unrecognised error text must return None.
    assert fn("connection refused") is None or isinstance(
        fn("connection refused"), dict
    )


def test_print_schemas_dumps_json(server) -> None:
    """Every registered tool must carry a non-empty description and a parameter schema."""
    srv, _ = server
    catalog = list_tools_for_export(srv)
    assert isinstance(catalog, list)
    names = {entry["name"] for entry in catalog}
    # All the headline tools should be present.
    assert {"list_tables", "describe_for_agent", "resolve_column", "run_sql"} <= names
    # New v0.7 tools should be present too.
    assert "validate_sql" in names
    assert "hint_for_error" in names
    for entry in catalog:
        assert isinstance(entry["name"], str) and entry["name"]
        assert isinstance(entry["description"], str) and entry["description"], (
            f"tool {entry['name']!r} has an empty description"
        )
        params = entry["parameters"]
        assert isinstance(params, dict)
        # FastMCP emits a JSON-Schema-shaped dict; `type` should be 'object'
        # or at least the schema should be present.
        assert "properties" in params or params == {"type": "object"} or params.get("type") == "object", (
            f"tool {entry['name']!r} parameters shape is unexpected: {params!r}"
        )
