"""failure_log smoke tests."""

from __future__ import annotations

import json
import sqlite3
from pathlib import Path

import pytest

from schemadex import SchemaCache, failure_log


def _seed(path: Path) -> None:
    conn = sqlite3.connect(path)
    conn.execute("CREATE TABLE users (id INTEGER PRIMARY KEY, email TEXT)")
    conn.commit()
    conn.close()


def test_wrap_records_run_sql_failure(tmp_path: Path) -> None:
    db = tmp_path / "demo.sqlite"
    _seed(db)
    base = SchemaCache.from_url(
        f"sqlite://{db}", cache_dir=str(tmp_path / "cache")
    )
    log_path = tmp_path / "failures.jsonl"
    cache, log = failure_log.wrap(base, path=str(log_path))
    assert log.path == log_path

    # Read-only guard rejects DELETE so the wrapper records it.
    url = f"sqlite://{db}"
    with pytest.raises(Exception):
        cache.run_sql(url, "DELETE FROM users", token_budget=100)

    records = failure_log.read(log_path)
    assert len(records) == 1
    rec = records[0]
    assert rec["sql"] == "DELETE FROM users"
    assert rec["error"]
    assert rec["fingerprint"] is not None
    parsed = json.loads(log_path.read_text().splitlines()[0])
    assert parsed["sql"] == "DELETE FROM users"

    # Proxy still forwards non-run_sql attrs.
    assert cache.list_tables() == base.list_tables()


def test_top_failure_modes_groups_by_error_prefix(tmp_path: Path) -> None:
    log_path = tmp_path / "failures.jsonl"
    log = failure_log.FailureLog(path=str(log_path))
    for _ in range(3):
        log.record("SELECT 1", "boom on connection X")
    log.record("SELECT 2", "other error")

    modes = failure_log.top_failure_modes(log_path, k=5)
    assert modes[0] == ("boom on connection X", 3)
    assert ("other error", 1) in modes
