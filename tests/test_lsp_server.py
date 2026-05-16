"""Smoke tests for the LSP server. Skip when pygls isn't installed."""

import pytest

pygls = pytest.importorskip("pygls.server")
from schemadex.lsp_server import build_server

import sqlite3
from pathlib import Path
from schemadex import SchemaCache


def _seed(path: Path):
    conn = sqlite3.connect(path)
    conn.execute("CREATE TABLE users (id INTEGER PRIMARY KEY, email TEXT, delay_code TEXT)")
    conn.commit()
    conn.close()


def test_build_server_with_completion(tmp_path: Path):
    db = tmp_path / "demo.sqlite"
    _seed(db)
    cache = SchemaCache.from_url(f"sqlite://{db}", cache_dir=str(tmp_path / "cache"))
    server = build_server(cache)
    assert server.name == "schemadex-lsp"
    assert callable(server.feature)
