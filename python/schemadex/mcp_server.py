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
"""

from __future__ import annotations

import argparse
import sys
from typing import Any

from mcp.server.fastmcp import FastMCP

from schemadex import SchemaCache


def build_server(url: str) -> FastMCP:
    cache = SchemaCache.from_url(url)
    mcp = FastMCP("schemadex")

    @mcp.tool()
    def list_tables() -> list[str]:
        """List every table in the connected database."""
        return cache.list_tables()

    @mcp.tool()
    def describe_for_agent(hint: str | None = None, max_tokens: int = 2048) -> str:
        """Return a token-budgeted schema description, optionally ranked by `hint`."""
        text, _tokens = cache.describe_for_agent(max_tokens=max_tokens, hint=hint)
        return text

    @mcp.tool()
    def resolve_column(table: str, candidate: str) -> dict[str, Any]:
        """Fuzzy-resolve a column name on a table."""
        r = cache.resolve(table, candidate)
        return {
            "matched": r.matched,
            "confidence": r.confidence,
            "alternatives": r.alternatives,
        }

    @mcp.tool()
    def run_sql(sql: str, token_budget: int = 1024) -> str:
        """Run a SQL query and return a markdown-rendered result table that fits `token_budget`."""
        text, _ = cache.run_sql(url, sql, token_budget=token_budget)
        return text

    return mcp


def main() -> int:
    ap = argparse.ArgumentParser(prog="schemadex-mcp")
    ap.add_argument(
        "--url",
        required=True,
        help="Database URL (postgres://..., sqlite://...)",
    )
    args = ap.parse_args()
    server = build_server(args.url)
    server.run()
    return 0


if __name__ == "__main__":
    sys.exit(main())
