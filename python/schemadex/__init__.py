"""schemadex — schema introspection and resolution toolkit for SQL agents.

Public API:

    from schemadex import SchemaCache, resolve, describe_for_agent

    cache = SchemaCache.from_url("postgres://localhost/mydb")
    print(cache.list_tables())
    table = cache.get_table("public.users")
    result = cache.resolve("public.users", "user_idd")
    schema_blurb, tokens = cache.describe_for_agent(max_tokens=2000, hint="orders")
"""

from __future__ import annotations

from ._native import ResolveResult, SchemaCache, __version__

__all__ = [
    "SchemaCache",
    "ResolveResult",
    "describe_for_agent",
    "resolve",
    "__version__",
]


def resolve(cache: SchemaCache, table: str, candidate: str) -> ResolveResult:
    """Fuzzy-resolve a candidate column name on a table.

    Convenience wrapper around ``SchemaCache.resolve``.
    """
    return cache.resolve(table, candidate)


def describe_for_agent(
    cache: SchemaCache,
    *,
    max_tokens: int = 2048,
    hint: str | None = None,
    tables: list[str] | None = None,
    include_samples: bool = True,
    include_foreign_keys: bool = True,
) -> tuple[str, int]:
    """Render a token-budgeted schema description and return ``(text, token_count)``."""
    return cache.describe_for_agent(
        max_tokens=max_tokens,
        hint=hint,
        tables=tables,
        include_samples=include_samples,
        include_foreign_keys=include_foreign_keys,
    )
