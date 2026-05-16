"""Tests for the optional /health and /metrics HTTP endpoints of the MCP server.

Skip the whole module if the optional ``mcp`` SDK isn't installed.
"""

from __future__ import annotations

import json
import sqlite3
import urllib.request
from pathlib import Path

import pytest

pytest.importorskip("mcp")
schemadex = pytest.importorskip("schemadex")

from schemadex.mcp_server import (  # noqa: E402
    _Metrics,
    build_server,
    start_metrics_server,
)


def _seed(path: Path) -> None:
    conn = sqlite3.connect(path)
    conn.executescript(
        """
        CREATE TABLE customers (id INTEGER PRIMARY KEY, email TEXT NOT NULL);
        INSERT INTO customers (email) VALUES ('a@x.com'), ('b@x.com');
        """
    )
    conn.commit()
    conn.close()


def _http_get(url: str) -> tuple[int, str]:
    with urllib.request.urlopen(url, timeout=5) as resp:  # noqa: S310 - test
        return resp.status, resp.read().decode("utf-8")


def test_health_endpoint_returns_ok_json(tmp_path: Path) -> None:
    db = tmp_path / "demo.sqlite"
    _seed(db)
    url = f"sqlite://{db}"

    metrics = _Metrics()
    server = build_server(url, metrics=metrics)
    cache = server._schemadex_cache  # type: ignore[attr-defined]
    httpd, port, _thread = start_metrics_server(cache, port=0, metrics=metrics)
    try:
        status, body = _http_get(f"http://127.0.0.1:{port}/health")
        assert status == 200
        payload = json.loads(body)
        assert payload["status"] == "ok"
        assert isinstance(payload["tables_in_cache"], int)
        # We seeded one table; the cache should reflect at least that.
        assert payload["tables_in_cache"] >= 1
    finally:
        httpd.shutdown()


def test_metrics_endpoint_exposes_prometheus_counters(tmp_path: Path) -> None:
    db = tmp_path / "demo.sqlite"
    _seed(db)
    url = f"sqlite://{db}"

    metrics = _Metrics()
    server = build_server(url, metrics=metrics)
    cache = server._schemadex_cache  # type: ignore[attr-defined]
    httpd, port, _thread = start_metrics_server(cache, port=0, metrics=metrics)
    try:
        status, body = _http_get(f"http://127.0.0.1:{port}/metrics")
        assert status == 200
        # All four headline metric names must be present.
        assert "schemadex_cache_tables" in body
        assert "schemadex_introspection_seconds_total" in body
        assert "schemadex_run_sql_calls_total" in body
        assert "schemadex_run_sql_errors_total" in body
    finally:
        httpd.shutdown()


def test_run_sql_tool_increments_counter(tmp_path: Path) -> None:
    """Calling the `run_sql` MCP tool should bump the run_sql_calls counter."""
    db = tmp_path / "demo.sqlite"
    _seed(db)
    url = f"sqlite://{db}"

    metrics = _Metrics()
    server = build_server(url, metrics=metrics)
    fn = server._tool_manager.get_tool("run_sql").fn
    assert metrics.snapshot()["run_sql_calls"] == 0
    fn("SELECT 1")
    assert metrics.snapshot()["run_sql_calls"] == 1
