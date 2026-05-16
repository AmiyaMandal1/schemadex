"""Build a schemadex SchemaCache from a dbt manifest.json.

`manifest.json` is dbt's compiled metadata: every model, source, column,
description, FK constraint (via `relationships` tests). When a project
has one, we skip live introspection entirely.

Usage:

    from schemadex import SchemaCache, dbt_source

    cache = dbt_source.from_manifest("path/to/target/manifest.json",
                                      cache_dir="/tmp/sd-cache")
"""

from __future__ import annotations

import json
import pathlib
import tempfile
from typing import Any

from schemadex import SchemaCache


def from_manifest(
    manifest_path: str | pathlib.Path,
    *,
    cache_dir: str | None = None,
) -> SchemaCache:
    """Load a dbt manifest.json, project it into a schemadex SchemaCache.

    Returns a SchemaCache backed by a synthetic SQLite database whose
    schema mirrors the manifest. Useful as a stand-in when the live
    warehouse isn't reachable (offline dev, CI, etc.).
    """
    raw = json.loads(pathlib.Path(manifest_path).read_text())
    nodes = raw.get("nodes", {})
    sources = raw.get("sources", {})

    # Collect (schema, name, columns) tuples from both nodes and sources.
    tables: list[tuple[str, str, list[dict[str, Any]]]] = []
    for node_id, node in {**nodes, **sources}.items():
        if not isinstance(node, dict):
            continue
        if node.get("resource_type") not in {"model", "source", "seed"}:
            continue
        schema = node.get("schema") or node.get("source_schema") or "main"
        name = node.get("name") or node.get("alias") or node_id.split(".")[-1]
        cols_dict = node.get("columns") or {}
        cols: list[dict[str, Any]] = []
        for col_name, col_meta in cols_dict.items():
            cols.append({
                "name": col_name,
                "type": (col_meta.get("data_type") or "TEXT").upper(),
                "comment": col_meta.get("description"),
            })
        tables.append((schema, name, cols))

    # Materialize into a SQLite database so we can drive the existing
    # SchemaCache.from_url path. This keeps the Rust core in charge of
    # parsing, fingerprinting, and caching.
    workdir = pathlib.Path(cache_dir) if cache_dir else pathlib.Path(tempfile.mkdtemp(prefix="schemadex_dbt_"))
    workdir.mkdir(parents=True, exist_ok=True)
    db_path = workdir / "dbt_synth.sqlite"
    if db_path.exists():
        db_path.unlink()

    import sqlite3

    conn = sqlite3.connect(db_path)
    try:
        for schema, name, cols in tables:
            if not cols:
                # SQLite needs at least one column.
                cols = [{"name": "_placeholder", "type": "TEXT"}]
            col_defs = ", ".join(
                f'"{c["name"]}" {c["type"]}' for c in cols
            )
            stmt = f'CREATE TABLE IF NOT EXISTS "{name}" ({col_defs});'
            conn.execute(stmt)
        conn.commit()
    finally:
        conn.close()

    url = f"sqlite://{db_path}"
    return SchemaCache.from_url(url, cache_dir=str(workdir / "cache"))
