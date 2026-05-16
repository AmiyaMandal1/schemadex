"""Quickstart: introspect a SQLite database and ask the agent describe API
for a token-budgeted prompt fragment.

Run::

    uv run python examples/quickstart.py
"""

from __future__ import annotations

import sqlite3
import tempfile
from pathlib import Path

from schemadex import SchemaCache


def seed_sqlite(path: Path) -> None:
    conn = sqlite3.connect(path)
    conn.executescript(
        """
        CREATE TABLE customers (
            id INTEGER PRIMARY KEY,
            email TEXT NOT NULL UNIQUE,
            region TEXT
        );
        CREATE TABLE orders (
            id INTEGER PRIMARY KEY,
            customer_id INTEGER NOT NULL REFERENCES customers(id),
            total_cents INTEGER NOT NULL,
            status TEXT NOT NULL
        );
        INSERT INTO customers VALUES
            (1, 'a@example.com', 'eu'),
            (2, 'b@example.com', 'us'),
            (3, 'c@example.com', 'us');
        INSERT INTO orders VALUES
            (1, 1, 999, 'paid'),
            (2, 2, 1299, 'paid'),
            (3, 3, 599, 'refunded');
        """
    )
    conn.commit()
    conn.close()


def main() -> None:
    with tempfile.TemporaryDirectory() as tmp:
        db = Path(tmp) / "demo.sqlite"
        seed_sqlite(db)
        url = f"sqlite://{db}"
        cache = SchemaCache.from_url(url, cache_dir=str(Path(tmp) / "cache"))

        print("tables:", cache.list_tables())

        result = cache.resolve("orders", "customerid")
        print("resolve('orders','customerid') ->", result.matched, result.confidence)

        prompt, tokens = cache.describe_for_agent(max_tokens=1024, hint="orders by region")
        print(f"-- {tokens} tokens --")
        print(prompt)


if __name__ == "__main__":
    main()
