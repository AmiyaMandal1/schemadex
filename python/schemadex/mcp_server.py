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
from typing import Annotated, Any

from mcp.server.fastmcp import FastMCP
from pydantic import Field

from schemadex import SchemaCache


def build_server(url: str) -> FastMCP:
    """Build a FastMCP server with the schemadex tools registered.

    Each tool ships with an explicit ``description`` and parameter schemas
    annotated via :class:`pydantic.Field`. The parameter metadata flows into
    the JSON Schema FastMCP emits for each tool, so agents that read the MCP
    tool catalog get a usable schema, not just ``{"type": "integer"}``.
    """
    cache = SchemaCache.from_url(url)
    mcp = FastMCP("schemadex")

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
        text, _ = cache.run_sql(url, sql, token_budget=token_budget)
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
    args = ap.parse_args()
    server = build_server(args.url)
    if args.print_schemas:
        print(json.dumps(list_tools_for_export(server), indent=2))
        return 0
    server.run()
    return 0


if __name__ == "__main__":
    sys.exit(main())
