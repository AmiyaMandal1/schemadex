"""Tests for the dbt manifest source.

We synthesize a minimal manifest.json (matching the shape dbt actually emits
for ``target/manifest.json``) and confirm that ``dbt_source.from_manifest``
projects the models into a usable ``SchemaCache``.
"""

from __future__ import annotations

import json
from pathlib import Path

import pytest

schemadex = pytest.importorskip("schemadex")
from schemadex import dbt_source  # noqa: E402


def _write_manifest(path: Path) -> Path:
    manifest = {
        "nodes": {
            "model.demo.customers": {
                "resource_type": "model",
                "name": "customers",
                "schema": "main",
                "alias": "customers",
                "columns": {
                    "id": {"name": "id", "data_type": "INTEGER", "description": "PK"},
                    "email": {"name": "email", "data_type": "TEXT", "description": "user email"},
                },
            },
            "model.demo.orders": {
                "resource_type": "model",
                "name": "orders",
                "schema": "main",
                "alias": "orders",
                "columns": {
                    "id": {"name": "id", "data_type": "INTEGER"},
                    "customer_id": {"name": "customer_id", "data_type": "INTEGER"},
                },
            },
        },
        "sources": {},
    }
    path.write_text(json.dumps(manifest))
    return path


def test_from_manifest_projects_tables(tmp_path: Path) -> None:
    manifest_path = _write_manifest(tmp_path / "manifest.json")
    cache = dbt_source.from_manifest(manifest_path, cache_dir=str(tmp_path / "work"))

    tables = cache.list_tables()
    assert "customers" in tables
    assert "orders" in tables


def test_from_manifest_columns(tmp_path: Path) -> None:
    manifest_path = _write_manifest(tmp_path / "manifest.json")
    cache = dbt_source.from_manifest(manifest_path, cache_dir=str(tmp_path / "work"))

    orders = cache.get_table("orders")
    assert orders is not None
    col_names = {c["name"] for c in orders["columns"]}
    assert {"id", "customer_id"} <= col_names


def test_from_manifest_handles_empty_columns(tmp_path: Path) -> None:
    """A model with no documented columns should still produce a table (with a
    placeholder) rather than crashing the SQLite materialization."""
    manifest = {
        "nodes": {
            "model.demo.empty_model": {
                "resource_type": "model",
                "name": "empty_model",
                "schema": "main",
                "columns": {},
            },
        },
        "sources": {},
    }
    manifest_path = tmp_path / "manifest.json"
    manifest_path.write_text(json.dumps(manifest))

    cache = dbt_source.from_manifest(manifest_path, cache_dir=str(tmp_path / "work"))
    assert "empty_model" in cache.list_tables()
