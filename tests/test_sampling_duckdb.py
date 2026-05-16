"""DuckDB sampling smoke test. Uses the in-tree `duckdb` Python package to
seed a file-based database with a dominant categorical value, then asks
schemadex to introspect it with `sample_values=True` and asserts the
sentinel fires on the dominant value.
"""

from __future__ import annotations

from pathlib import Path

import pytest

schemadex = pytest.importorskip("schemadex")
duckdb = pytest.importorskip("duckdb")


def seed(path: Path) -> None:
    """Mirror the SQLite smoke seed: 4 of 5 rows share `delay_code = 'No Delay'`
    so the sentinel threshold fires at 80%.
    """
    conn = duckdb.connect(str(path))
    try:
        conn.execute(
            """
            CREATE TABLE users (
                id INTEGER PRIMARY KEY,
                email VARCHAR NOT NULL,
                delay_code VARCHAR
            );
            """
        )
        conn.executemany(
            "INSERT INTO users (id, email, delay_code) VALUES (?, ?, ?)",
            [
                (1, "a@x.com", "No Delay"),
                (2, "b@x.com", "No Delay"),
                (3, "c@x.com", "No Delay"),
                (4, "d@x.com", "No Delay"),
                (5, "e@x.com", "Backhaul"),
            ],
        )
    finally:
        conn.close()


def test_duckdb_sampling_populates_sample(tmp_path: Path) -> None:
    db = tmp_path / "demo.duckdb"
    seed(db)
    cache = schemadex.SchemaCache.from_url(
        f"duckdb://{db}",
        cache_dir=str(tmp_path / "cache"),
        sample_values=True,
        sample_top_k=5,
        sample_sentinel_threshold=0.4,
        sample_rows=1000,
    )
    table = cache.get_table("users")
    assert table is not None, "duckdb introspection should surface users"
    delay = next(c for c in table["columns"] if c["name"] == "delay_code")
    sample = delay.get("sample")
    assert sample is not None, "duckdb sampling should populate sample"
    sentinel = sample.get("sentinel")
    assert sentinel is not None, "sentinel should fire at 80% No Delay"
    assert sentinel[0] == "No Delay"
    assert sentinel[1] > 0.7
