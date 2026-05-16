"""Tests for the IPython magic wiring.

The magic just delegates to ``SchemaCache.from_url`` and a few methods, so we
focus on (a) that the extension registers cleanly with an IPython instance,
and (b) that ``_schemadex`` returns sensible values when invoked directly.
"""

from __future__ import annotations

import sqlite3
from pathlib import Path

import pytest

pytest.importorskip("IPython")

schemadex = pytest.importorskip("schemadex")
from schemadex import jupyter  # noqa: E402


def _seed(db: Path) -> None:
    conn = sqlite3.connect(db)
    conn.executescript(
        """
        CREATE TABLE customers (
            id INTEGER PRIMARY KEY,
            email TEXT NOT NULL
        );
        CREATE TABLE orders (
            id INTEGER PRIMARY KEY,
            customer_id INTEGER NOT NULL,
            amount REAL
        );
        """
    )
    conn.commit()
    conn.close()


@pytest.fixture(autouse=True)
def _reset_jupyter_state() -> None:
    """The magic keeps a module-level URL/cache. Reset between tests so one
    test's seeded sqlite path doesn't leak into the next."""
    jupyter._DEFAULT_URL = None
    jupyter._CACHE = None
    yield
    jupyter._DEFAULT_URL = None
    jupyter._CACHE = None


def test_load_ipython_extension_registers(tmp_path: Path) -> None:
    from IPython.testing.globalipapp import get_ipython

    ip = get_ipython()
    # Calling via the package-level entry point exercises the same path
    # ``%load_ext schemadex`` would.
    schemadex.load_ipython_extension(ip)
    # The magics should now be present.
    line_magics = ip.magics_manager.magics["line"]
    assert "schemadex" in line_magics
    assert "schemadex_url" in line_magics


def test_schemadex_list_tables(tmp_path: Path) -> None:
    db = tmp_path / "demo.sqlite"
    _seed(db)
    result = jupyter._schemadex(f"--url sqlite://{db} list-tables")
    assert isinstance(result, list)
    assert "customers" in result
    assert "orders" in result


def test_schemadex_describe_table(tmp_path: Path) -> None:
    db = tmp_path / "demo.sqlite"
    _seed(db)
    result = jupyter._schemadex(f"--url sqlite://{db} describe orders")
    assert isinstance(result, dict)
    col_names = {c["name"] for c in result["columns"]}
    assert {"id", "customer_id", "amount"} <= col_names


def test_schemadex_resolve(tmp_path: Path) -> None:
    db = tmp_path / "demo.sqlite"
    _seed(db)
    result = jupyter._schemadex(f"--url sqlite://{db} resolve orders customerid")
    # ResolveResult has a `matched` attribute.
    assert result.matched == "customer_id"


def test_schemadex_url_then_default(tmp_path: Path) -> None:
    db = tmp_path / "demo.sqlite"
    _seed(db)
    msg = jupyter._schemadex_url(f"sqlite://{db}")
    assert "schemadex URL set" in msg
    tables = jupyter._schemadex("list-tables")
    assert "customers" in tables


def test_schemadex_usage_when_empty(tmp_path: Path) -> None:
    result = jupyter._schemadex("")
    assert "usage" in result


def test_schemadex_unknown_verb(tmp_path: Path) -> None:
    db = tmp_path / "demo.sqlite"
    _seed(db)
    result = jupyter._schemadex(f"--url sqlite://{db} bogus")
    assert "unknown verb" in result
