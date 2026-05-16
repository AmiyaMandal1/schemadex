"""Tests for the schemadex local web dashboard.

We bind to port 0 (random free port), serve from a background thread, and
poke each route over real HTTP. That exercises the full handler — routing,
URL parsing, JSON encoding — rather than calling ``do_GET`` in isolation.
"""

from __future__ import annotations

import http.client
import json
import sqlite3
import threading
from http.server import HTTPServer
from pathlib import Path
from typing import Iterator

import pytest

from schemadex import SchemaCache
from schemadex.web import _make_handler


def _seed(path: Path) -> None:
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
            ('c@x.com', 'Backhaul');
        """
    )
    conn.commit()
    conn.close()


@pytest.fixture()
def server(tmp_path: Path) -> Iterator[tuple[str, int]]:
    db = tmp_path / "demo.sqlite"
    _seed(db)
    url = f"sqlite://{db}"
    cache = SchemaCache.from_url(url, cache_dir=str(tmp_path / "cache"))
    handler_cls = _make_handler(cache)
    httpd = HTTPServer(("127.0.0.1", 0), handler_cls)
    port = httpd.server_address[1]
    thread = threading.Thread(target=httpd.serve_forever, daemon=True)
    thread.start()
    try:
        yield "127.0.0.1", port
    finally:
        httpd.shutdown()
        httpd.server_close()
        thread.join(timeout=2)


def _get(host: str, port: int, path: str) -> tuple[int, str, str]:
    conn = http.client.HTTPConnection(host, port, timeout=5)
    try:
        conn.request("GET", path)
        resp = conn.getresponse()
        body = resp.read().decode("utf-8")
        ctype = resp.getheader("Content-Type", "")
        return resp.status, ctype, body
    finally:
        conn.close()


def test_index_html(server) -> None:
    host, port = server
    status, ctype, body = _get(host, port, "/")
    assert status == 200
    assert "text/html" in ctype
    assert "schemadex" in body
    # Sanity-check that the page actually carries the JS bootstrap, not just
    # the title — otherwise we've regressed to a stub.
    assert "/api/tables" in body


def test_api_tables_returns_json_list(server) -> None:
    host, port = server
    status, ctype, body = _get(host, port, "/api/tables")
    assert status == 200
    assert "application/json" in ctype
    data = json.loads(body)
    assert isinstance(data, list)
    assert "users" in data


def test_api_table_returns_json(server) -> None:
    host, port = server
    status, ctype, body = _get(host, port, "/api/table/users")
    assert status == 200
    assert "application/json" in ctype
    data = json.loads(body)
    assert isinstance(data, dict)
    # The cache exposes the table as a dict with name + columns.
    assert data.get("name") == "users"
    col_names = {c["name"] for c in data["columns"]}
    assert "delay_code" in col_names


def test_api_resolve_matches_fuzzy(server) -> None:
    host, port = server
    status, ctype, body = _get(
        host, port, "/api/resolve?table=users&candidate=delaycode"
    )
    assert status == 200
    assert "application/json" in ctype
    data = json.loads(body)
    assert set(data.keys()) == {"matched", "confidence", "alternatives"}
    assert data["matched"] == "delay_code"
    assert isinstance(data["confidence"], float)
    assert isinstance(data["alternatives"], list)


def test_api_resolve_missing_params_400(server) -> None:
    host, port = server
    status, _ctype, body = _get(host, port, "/api/resolve?table=users")
    assert status == 400
    data = json.loads(body)
    assert "error" in data


def test_unknown_route_404(server) -> None:
    host, port = server
    status, _ctype, _body = _get(host, port, "/nope")
    assert status == 404
