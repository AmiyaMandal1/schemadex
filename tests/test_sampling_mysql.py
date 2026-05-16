"""MySQL sampling smoke test. Skipped unless ``DATABASE_URL_MYSQL`` is set
to a writable MySQL URL (e.g. a docker-compose service in CI). The test
creates a throwaway table, seeds 4-of-5 dominant rows, and asserts the
sentinel fires through schemadex's MySQL backend.
"""

from __future__ import annotations

import os
import uuid
from pathlib import Path

import pytest

schemadex = pytest.importorskip("schemadex")

DATABASE_URL = os.environ.get("DATABASE_URL_MYSQL")

pytestmark = pytest.mark.skipif(
    not DATABASE_URL,
    reason="DATABASE_URL_MYSQL not set — skipping MySQL sampling smoke test",
)


def _connect_url_to_sqlalchemy(url: str) -> str:
    # schemadex accepts mysql://... ; PyMySQL needs mysql+pymysql://...
    if url.startswith("mysql://"):
        return "mysql+pymysql://" + url[len("mysql://") :]
    return url


def test_mysql_sampling_populates_sample(tmp_path: Path) -> None:
    pymysql = pytest.importorskip("pymysql")
    # Parse out connection params from the URL via a tiny urllib helper.
    from urllib.parse import urlparse

    parsed = urlparse(DATABASE_URL)
    table = f"schemadex_sample_{uuid.uuid4().hex[:8]}"
    conn = pymysql.connect(
        host=parsed.hostname or "127.0.0.1",
        port=parsed.port or 3306,
        user=parsed.username or "root",
        password=parsed.password or "",
        database=(parsed.path or "/").lstrip("/") or "test",
    )
    try:
        with conn.cursor() as cur:
            cur.execute(
                f"CREATE TABLE `{table}` (id INT PRIMARY KEY, email VARCHAR(255) NOT NULL, delay_code VARCHAR(64))"
            )
            cur.executemany(
                f"INSERT INTO `{table}` (id, email, delay_code) VALUES (%s, %s, %s)",
                [
                    (1, "a@x.com", "No Delay"),
                    (2, "b@x.com", "No Delay"),
                    (3, "c@x.com", "No Delay"),
                    (4, "d@x.com", "No Delay"),
                    (5, "e@x.com", "Backhaul"),
                ],
            )
        conn.commit()

        cache = schemadex.SchemaCache.from_url(
            DATABASE_URL,
            cache_dir=str(tmp_path / "cache"),
            sample_values=True,
            sample_top_k=5,
            sample_sentinel_threshold=0.4,
            sample_rows=1000,
        )
        table_meta = cache.get_table(table)
        assert table_meta is not None, f"mysql introspection should surface {table}"
        delay = next(c for c in table_meta["columns"] if c["name"] == "delay_code")
        sample = delay.get("sample")
        assert sample is not None, "mysql sampling should populate sample"
        sentinel = sample.get("sentinel")
        assert sentinel is not None, "sentinel should fire at 80% No Delay"
        assert sentinel[0] == "No Delay"
        assert sentinel[1] > 0.7
    finally:
        with conn.cursor() as cur:
            cur.execute(f"DROP TABLE IF EXISTS `{table}`")
        conn.commit()
        conn.close()
