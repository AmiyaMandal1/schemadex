"""schemadex MCP server.

Run via:

    schemadex-mcp --url postgres://localhost/mydb

In Claude Code's ~/.claude/mcp.json:

    {
      "mcpServers": {
        "schemadex": {
          "command": "schemadex-mcp",
          "args": ["--url", "sqlite:///path/to/db.sqlite"]
        }
      }
    }

You can also dump the JSON Schema for each registered tool — useful for
agents that don't speak MCP but still want to consume the tool surface:

    schemadex-mcp --url ... --print-schemas
"""

from __future__ import annotations

import argparse
import json
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, HTTPServer
from typing import Annotated, Any

from mcp.server.fastmcp import FastMCP
from pydantic import Field

from schemadex import SchemaCache


class _Metrics:
    """Tiny thread-safe counter store for the optional ops endpoint.

    Stdlib-only so we don't drag in `prometheus_client` for what amounts to
    four counters. Each method takes the lock for the duration of a single
    integer increment / read, which is cheap enough that the contention cost
    is negligible compared to the SQL roundtrip it accompanies.
    """

    def __init__(self) -> None:
        self._lock = threading.Lock()
        self.run_sql_calls: int = 0
        self.run_sql_errors: int = 0
        self.introspection_seconds_total: float = 0.0
        # Snapshot of `len(cache.list_tables())` last time we asked. Refreshed
        # on every `/metrics` and `/health` hit so it tracks refreshes that
        # happen after the server started.
        self._tables_in_cache: int = 0

    def inc_run_sql(self) -> None:
        with self._lock:
            self.run_sql_calls += 1

    def inc_run_sql_error(self) -> None:
        with self._lock:
            self.run_sql_errors += 1

    def add_introspection_time(self, seconds: float) -> None:
        with self._lock:
            self.introspection_seconds_total += seconds

    def set_tables_in_cache(self, n: int) -> None:
        with self._lock:
            self._tables_in_cache = n

    def snapshot(self) -> dict[str, Any]:
        with self._lock:
            return {
                "run_sql_calls": self.run_sql_calls,
                "run_sql_errors": self.run_sql_errors,
                "introspection_seconds_total": self.introspection_seconds_total,
                "tables_in_cache": self._tables_in_cache,
            }


# One module-level instance; the build_server callable shares this with the
# HTTP listener spawned by start_metrics_server. Tests reset it between cases
# by reaching into the attribute directly.
_metrics = _Metrics()


def _make_handler(cache: SchemaCache, metrics: _Metrics):
    """Build a BaseHTTPRequestHandler subclass that closes over `cache`/`metrics`.

    Routes:
      - GET /health   -> 200 + {"status":"ok","tables_in_cache": N}
      - GET /metrics  -> 200 + Prometheus text exposition
    Anything else returns 404.
    """

    class Handler(BaseHTTPRequestHandler):
        def _refresh_table_count(self) -> int:
            try:
                n = len(cache.list_tables())
            except Exception:  # pragma: no cover - defensive
                n = 0
            metrics.set_tables_in_cache(n)
            return n

        def do_GET(self) -> None:  # noqa: N802 - stdlib signature
            if self.path == "/health":
                n = self._refresh_table_count()
                body = json.dumps(
                    {"status": "ok", "tables_in_cache": n}
                ).encode("utf-8")
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)
                return
            if self.path == "/metrics":
                self._refresh_table_count()
                snap = metrics.snapshot()
                lines = [
                    "# HELP schemadex_cache_tables Tables currently in the schema cache.",
                    "# TYPE schemadex_cache_tables gauge",
                    f"schemadex_cache_tables {snap['tables_in_cache']}",
                    "# HELP schemadex_introspection_seconds_total Cumulative time spent in introspection.",
                    "# TYPE schemadex_introspection_seconds_total counter",
                    f"schemadex_introspection_seconds_total {snap['introspection_seconds_total']}",
                    "# HELP schemadex_run_sql_calls_total Number of run_sql invocations.",
                    "# TYPE schemadex_run_sql_calls_total counter",
                    f"schemadex_run_sql_calls_total {snap['run_sql_calls']}",
                    "# HELP schemadex_run_sql_errors_total Number of run_sql errors.",
                    "# TYPE schemadex_run_sql_errors_total counter",
                    f"schemadex_run_sql_errors_total {snap['run_sql_errors']}",
                    "",
                ]
                body = "\n".join(lines).encode("utf-8")
                self.send_response(200)
                self.send_header(
                    "Content-Type", "text/plain; version=0.0.4; charset=utf-8"
                )
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)
                return
            self.send_response(404)
            self.end_headers()

        def log_message(self, format: str, *args: Any) -> None:  # noqa: A002
            # Silence the default stderr access log — MCP runs over stdio and
            # the chatter would leak into the protocol stream if a user pipes
            # stderr around.
            return

    return Handler


def start_metrics_server(
    cache: SchemaCache,
    port: int,
    metrics: _Metrics | None = None,
) -> tuple[HTTPServer, int, threading.Thread]:
    """Start an HTTP listener on `port` exposing /health and /metrics.

    Pass ``port=0`` to bind a random free port (useful in tests). Returns
    ``(server, bound_port, thread)``; the thread is already daemonized and
    running. Call ``server.shutdown()`` to stop it cleanly.
    """
    m = metrics if metrics is not None else _metrics
    handler_cls = _make_handler(cache, m)
    httpd = HTTPServer(("127.0.0.1", port), handler_cls)
    bound_port = httpd.server_address[1]
    thread = threading.Thread(
        target=httpd.serve_forever, name="schemadex-metrics", daemon=True
    )
    thread.start()
    return httpd, bound_port, thread


def build_server(url: str, metrics: _Metrics | None = None) -> FastMCP:
    """Build a FastMCP server with the schemadex tools registered.

    Each tool ships with an explicit ``description`` and parameter schemas
    annotated via :class:`pydantic.Field`. The parameter metadata flows into
    the JSON Schema FastMCP emits for each tool, so agents that read the MCP
    tool catalog get a usable schema, not just ``{"type": "integer"}``.

    Pass an explicit ``metrics`` instance to share counters with an external
    HTTP listener (see :func:`start_metrics_server`). The default uses the
    module-level singleton so a stdio-only deployment still gets accurate
    counts if the ops endpoint is enabled later.
    """
    m = metrics if metrics is not None else _metrics
    introspect_start = time.perf_counter()
    cache = SchemaCache.from_url(url)
    m.add_introspection_time(time.perf_counter() - introspect_start)
    try:
        m.set_tables_in_cache(len(cache.list_tables()))
    except Exception:  # pragma: no cover - defensive
        pass
    mcp = FastMCP("schemadex")
    # Expose the cache on the server object so callers (and tests) can grab
    # it without re-deriving it from the URL.
    mcp._schemadex_cache = cache  # type: ignore[attr-defined]
    mcp._schemadex_metrics = m  # type: ignore[attr-defined]

    @mcp.tool(description="List every table in the connected database.")
    def list_tables() -> list[str]:
        return cache.list_tables()

    @mcp.tool(
        description=(
            "Render a token-budgeted schema description. Use the `hint` "
            "parameter to bias table ranking toward the user's question. "
            "Returns the rendered text. Tables, columns, primary keys, "
            "foreign keys, and (when available) sample values are included."
        ),
    )
    def describe_for_agent(
        hint: Annotated[
            str | None,
            Field(
                description=(
                    "Free-text hint like 'orders by region' that biases "
                    "ranking. The top-scoring tables are kept; low-ranked "
                    "ones are dropped first when the budget is tight."
                ),
                examples=["orders by region", "users with email"],
            ),
        ] = None,
        max_tokens: Annotated[
            int,
            Field(
                description=(
                    "Maximum tokens in the response. Truncates samples "
                    "first, then comments, then FKs, then drops low-ranked "
                    "tables."
                ),
                ge=256,
                le=16384,
            ),
        ] = 2048,
    ) -> str:
        text, _tokens = cache.describe_for_agent(max_tokens=max_tokens, hint=hint)
        return text

    @mcp.tool(
        description=(
            "Fuzzy-resolve a column name on a table. Returns the best match "
            "plus up to three alternatives, each scored in [0.0, 1.0]. Use "
            "this when the user/agent invents a column name that may not "
            "exist verbatim (e.g. 'delaycode' -> 'delay_code')."
        ),
    )
    def resolve_column(
        table: Annotated[
            str,
            Field(
                description=(
                    "Real or qualified table name (e.g. 'users' or "
                    "'public.users'). Matching is case-insensitive."
                ),
            ),
        ],
        candidate: Annotated[
            str,
            Field(
                description="The candidate column name to resolve.",
                examples=["delaycode", "user_idd"],
            ),
        ],
    ) -> dict[str, Any]:
        r = cache.resolve(table, candidate)
        return {
            "matched": r.matched,
            "confidence": r.confidence,
            "alternatives": r.alternatives,
        }

    @mcp.tool(
        description=(
            "Run a read-only SQL query and return a markdown-rendered "
            "result table that fits inside `token_budget`. Only SELECT / "
            "WITH / EXPLAIN / SHOW / DESCRIBE / DESC statements are accepted."
        ),
    )
    def run_sql(
        sql: Annotated[
            str,
            Field(
                description="The SQL query to execute. Must be read-only.",
                examples=["SELECT id, email FROM users ORDER BY id LIMIT 10"],
            ),
        ],
        token_budget: Annotated[
            int,
            Field(
                description=(
                    "Maximum tokens in the rendered result. Rows are "
                    "dropped from the bottom until the table fits; if "
                    "anything was dropped a `_(truncated to N rows)_` "
                    "marker is appended."
                ),
                ge=128,
                le=16384,
            ),
        ] = 1024,
    ) -> str:
        m.inc_run_sql()
        try:
            text, _ = cache.run_sql(url, sql, token_budget=token_budget)
        except Exception:
            m.inc_run_sql_error()
            raise
        return text

    @mcp.tool(
        description=(
            "Pre-validate a SQL query against the cached schema without "
            "executing it. Returns a list of issues; an empty list means "
            "the query looks safe to run. Each issue carries a `kind` "
            "(unknown_table / unknown_column), the offending identifier, "
            "and (when possible) a fuzzy suggestion."
        ),
    )
    def validate_sql(
        sql: Annotated[
            str,
            Field(
                description=(
                    "The SQL query to pre-validate. Validation is "
                    "heuristic and regex-based — it catches typos in "
                    "table/column names but is not a full SQL parser."
                ),
                examples=["SELECT emial FROM users"],
            ),
        ],
    ) -> list[dict[str, Any]]:
        # Graceful degradation: if the parallel agent's `validate_sql`
        # method hasn't landed yet, return an empty list rather than
        # crashing the MCP session. Once the method exists the happy path
        # kicks in.
        fn = getattr(cache, "validate_sql", None)
        if fn is None:
            return []
        issues = fn(sql)
        # The native method may return JSON-serialisable dicts directly,
        # or pydantic-y objects with attributes. Normalise to a list of
        # plain dicts so the MCP layer always emits stable JSON.
        return [_to_plain_dict(i) for i in issues]

    @mcp.tool(
        description=(
            "Wrap a raw database error message in a structured hint. When "
            "the engine returns `column \"emial\" does not exist`, the "
            "hint surfaces the likely-correct identifier ('email') so the "
            "agent can retry without guessing. Returns None if the error "
            "text doesn't match a known pattern."
        ),
    )
    def hint_for_error(
        error_message: Annotated[
            str,
            Field(
                description="The raw database error message.",
                examples=[
                    'column "emial" does not exist',
                    "no such table: userz",
                ],
            ),
        ],
    ) -> dict[str, Any] | None:
        fn = getattr(cache, "hint_for_error", None)
        if fn is None:
            return None
        hint = fn(error_message)
        if hint is None:
            return None
        return _to_plain_dict(hint)

    return mcp


def _to_plain_dict(obj: Any) -> dict[str, Any]:
    """Coerce a tool return value into a plain dict.

    The native bindings may return dicts directly, pydantic models, or
    objects with attributes — accept all three so the MCP envelope stays
    stable.
    """
    if isinstance(obj, dict):
        return obj
    if hasattr(obj, "model_dump"):
        return obj.model_dump()
    if hasattr(obj, "__dict__"):
        return dict(obj.__dict__)
    return {"value": obj}


def list_tools_for_export(mcp: FastMCP) -> list[dict[str, Any]]:
    """Dump the registered tool catalog as a list of JSON-friendly entries.

    Each entry has ``name``, ``description``, and ``parameters`` (a JSON
    Schema). Useful for agents that consume tools via a static catalog
    rather than the MCP protocol — they can read this once and call the
    same Python entry points themselves.
    """
    out: list[dict[str, Any]] = []
    for tool in mcp._tool_manager.list_tools():
        out.append(
            {
                "name": tool.name,
                "description": tool.description or "",
                "parameters": tool.parameters,
            }
        )
    return out


def main() -> int:
    ap = argparse.ArgumentParser(prog="schemadex-mcp")
    ap.add_argument(
        "--url",
        required=True,
        help="Database URL (postgres://..., sqlite://...)",
    )
    ap.add_argument(
        "--print-schemas",
        action="store_true",
        help=(
            "Print the registered MCP tool catalog as JSON to stdout and "
            "exit. Useful for agents that consume tools via a static "
            "catalog rather than the MCP protocol."
        ),
    )
    ap.add_argument(
        "--metrics-port",
        type=int,
        default=None,
        help=(
            "Start an HTTP listener on the given port exposing /health "
            "and /metrics (Prometheus text format). Pass 0 for a random "
            "free port; useful for ops dashboards and Kubernetes probes."
        ),
    )
    args = ap.parse_args()
    server = build_server(args.url)
    if args.print_schemas:
        print(json.dumps(list_tools_for_export(server), indent=2))
        return 0
    if args.metrics_port is not None:
        cache = server._schemadex_cache  # type: ignore[attr-defined]
        metrics = server._schemadex_metrics  # type: ignore[attr-defined]
        _httpd, bound_port, _thread = start_metrics_server(
            cache, args.metrics_port, metrics=metrics
        )
        # Stderr so it doesn't pollute the MCP stdio protocol channel.
        print(
            f"schemadex-mcp metrics listening on http://127.0.0.1:{bound_port}",
            file=sys.stderr,
        )
    server.run()
    return 0


if __name__ == "__main__":
    sys.exit(main())
