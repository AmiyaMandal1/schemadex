"""CDC-triggered cache invalidation sketch.

This example shows how to wire a Postgres logical-replication consumer into
:class:`schemadex.SchemaCache` so DDL changes invalidate just the affected
tables instead of waiting for the next TTL expiry.

It is intentionally a sketch — schemadex doesn't bundle a replication
client. The plumbing below uses ``psycopg`` (v3) and assumes the operator
has:

1. Set ``wal_level = logical`` in ``postgresql.conf``.
2. Created a replication slot::

       SELECT pg_create_logical_replication_slot('schemadex_ddl', 'pgoutput');

3. Created a publication that fires on DDL (Postgres 14+ via event
   triggers — there's no built-in DDL publication, so most setups install
   ``pglogical`` or rely on event triggers writing into a heartbeat
   table). The exact mechanism varies; this example just shows what to do
   with the events once you have them.

Run it alongside your application after warming the cache::

    python examples/postgres_logical_replication.py \\
        postgres://user:pw@localhost:5432/mydb

The script reads the replication stream, parses each event into a
``(operation, qualified_table_name)`` pair, and calls
``cache.invalidate_table(...)`` whenever it sees a DDL or DML event
against a table schemadex knows about.
"""

from __future__ import annotations

import os
import sys
import time
from dataclasses import dataclass
from typing import Iterator

import schemadex


@dataclass
class DDLEvent:
    """A simplified DDL/DML event delivered by the replication stream.

    Real ``pgoutput`` decoding produces a richer payload (operation kind,
    relation OID, tuple data, etc.). For the purposes of invalidation we
    only need ``op`` and ``qualified_name``.
    """

    op: str  # e.g. "ALTER", "DROP", "CREATE", "INSERT", "UPDATE", "DELETE"
    qualified_name: str  # e.g. "public.users"


def stream_events(dsn: str) -> Iterator[DDLEvent]:
    """Yield :class:`DDLEvent` instances forever.

    The real implementation would open a replication connection with
    ``psycopg.Connection.connect(dsn, autocommit=True)`` and call
    ``cursor.start_replication(slot_name="schemadex_ddl", decode=True)``,
    then loop over the resulting message stream. Decoding ``pgoutput``
    bytes into a structured record is several hundred lines of code that
    isn't relevant to schemadex itself.

    This sketch yields a single demonstration event and exits so the
    example runs end-to-end without a live database.
    """
    # NOTE: replace this body with real psycopg replication code.
    yield DDLEvent(op="ALTER", qualified_name="public.users")


def main(argv: list[str]) -> int:
    if len(argv) != 2:
        print(__doc__, file=sys.stderr)
        return 2
    dsn = argv[1]

    # Warm or load the cache; the application has presumably already done
    # this at startup. We re-load (no introspection) to avoid hammering
    # the DB just for the demo.
    cache = schemadex.SchemaCache.load(dsn) or schemadex.SchemaCache.from_url(dsn)
    print(f"watching {len(cache.list_tables())} tables for DDL changes")

    known = {t.lower() for t in cache.list_tables()}
    for event in stream_events(dsn):
        qn = event.qualified_name.lower()
        if qn not in known and qn.split(".")[-1] not in known:
            # Either a brand-new table (we'll catch it on the next full
            # refresh) or something outside our cached schema. Skip.
            continue
        try:
            cache.invalidate_table(event.qualified_name)
            print(
                f"[{time.strftime('%H:%M:%S')}] invalidated "
                f"{event.qualified_name} on {event.op}"
            )
        except Exception as exc:
            # Don't let a transient cache error kill the replication loop.
            print(
                f"[{time.strftime('%H:%M:%S')}] invalidate failed for "
                f"{event.qualified_name}: {exc}",
                file=sys.stderr,
            )
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))


# Avoid a startup warning when the file is imported (e.g. by docs builders)
# without a database connection.
_ = os.environ.get("SCHEMADEX_LOGICAL_REPL_DSN")
