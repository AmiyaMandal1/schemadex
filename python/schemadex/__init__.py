"""schemadex — schema introspection and resolution toolkit for SQL agents.

Public API:

    from schemadex import SchemaCache, resolve, describe_for_agent

    cache = SchemaCache.from_url("postgres://localhost/mydb")
    print(cache.list_tables())
    table = cache.get_table("public.users")
    result = cache.resolve("public.users", "user_idd")
    schema_blurb, tokens = cache.describe_for_agent(max_tokens=2000, hint="orders")

Async variants are also available for use from inside an ``asyncio`` event
loop — they share the same underlying tokio runtime as the sync API and do not
block the event loop thread:

    import asyncio
    from schemadex import from_url_async, run_sql_async

    async def main():
        cache = await from_url_async("sqlite:///tmp/demo.sqlite")
        rendered, tokens = await run_sql_async(cache, url, "SELECT 1")
"""

from __future__ import annotations

from . import _native
from ._native import ResolveResult, SchemaCache, __version__
from .embedding_resolve import resolve_with_embedding
from . import dbt_source

__all__ = [
    "SchemaCache",
    "ResolveResult",
    "describe_for_agent",
    "resolve",
    "resolve_with_embedding",
    "from_url_async",
    "refresh_async",
    "refresh_table_async",
    "run_sql_async",
    "dbt_source",
    "__version__",
]


def load_ipython_extension(ipython):
    """IPython entry point: makes ``%load_ext schemadex`` work."""
    from . import jupyter
    jupyter.load_ipython_extension(ipython)


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
    include_examples: bool = False,
) -> tuple[str, int]:
    """Render a token-budgeted schema description and return ``(text, token_count)``.

    Set ``include_examples=True`` to append a short list of generated
    few-shot SELECT statements per table. Examples are dropped before
    comments when the budget is tight.
    """
    return cache.describe_for_agent(
        max_tokens=max_tokens,
        hint=hint,
        tables=tables,
        include_samples=include_samples,
        include_foreign_keys=include_foreign_keys,
        include_examples=include_examples,
    )


async def from_url_async(
    url: str,
    *,
    ttl_seconds: int | None = None,
    cache_dir: str | None = None,
    parallel: bool = True,
    sample_values: bool = False,
    sample_top_k: int | None = None,
    sample_sentinel_threshold: float | None = None,
    sample_rows: int | None = None,
    history: int | None = None,
    max_history: int = 10,
    memoize_results: bool = False,
    memo_capacity: int = 128,
) -> SchemaCache:
    """Async variant of :meth:`SchemaCache.from_url`.

    Builds a :class:`SchemaCache` by introspecting ``url`` without blocking
    the calling event-loop thread. The kwargs mirror :meth:`SchemaCache.from_url`.
    """
    return await _native.from_url_async(
        url,
        ttl_seconds=ttl_seconds,
        cache_dir=cache_dir,
        parallel=parallel,
        sample_values=sample_values,
        sample_top_k=sample_top_k,
        sample_sentinel_threshold=sample_sentinel_threshold,
        sample_rows=sample_rows,
        history=history,
        max_history=max_history,
        memoize_results=memoize_results,
        memo_capacity=memo_capacity,
    )


async def refresh_async(
    cache: SchemaCache,
    url: str,
    *,
    sample_values: bool = False,
    sample_top_k: int | None = None,
    sample_sentinel_threshold: float | None = None,
    sample_rows: int | None = None,
    parallel: bool = True,
) -> tuple[list[str], list[str]]:
    """Async variant of :meth:`SchemaCache.refresh`. Returns ``(changed, unchanged)``."""
    return await _native.refresh_async(
        cache,
        url,
        sample_values=sample_values,
        sample_top_k=sample_top_k,
        sample_sentinel_threshold=sample_sentinel_threshold,
        sample_rows=sample_rows,
        parallel=parallel,
    )


async def refresh_table_async(
    cache: SchemaCache,
    url: str,
    table: str,
    *,
    sample_values: bool = False,
    sample_top_k: int | None = None,
    sample_sentinel_threshold: float | None = None,
    sample_rows: int | None = None,
) -> tuple[list[str], list[str]]:
    """Async variant of :meth:`SchemaCache.refresh_table`. Returns ``(changed, unchanged)``."""
    return await _native.refresh_table_async(
        cache,
        url,
        table,
        sample_values=sample_values,
        sample_top_k=sample_top_k,
        sample_sentinel_threshold=sample_sentinel_threshold,
        sample_rows=sample_rows,
    )


async def run_sql_async(
    cache: SchemaCache,
    url: str,
    sql: str,
    token_budget: int = 1024,
    allow_write: bool = False,
    memoize: bool = False,
) -> tuple[str, int]:
    """Async variant of :meth:`SchemaCache.run_sql`. Returns ``(rendered, token_count)``.

    ``allow_write=True`` skips the read-only SQL guard. Only do this if you
    have already validated the SQL yourself — write statements (INSERT,
    UPDATE, DELETE, DROP, ...) will reach the database.

    ``memoize=True`` opts into the LRU result cache (no-op unless the cache
    was constructed with ``memoize_results=True``).
    """
    return await _native.run_sql_async(
        cache, url, sql, token_budget, allow_write=allow_write, memoize=memoize
    )
