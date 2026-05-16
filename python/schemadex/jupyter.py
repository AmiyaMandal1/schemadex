"""IPython magic for schemadex.

Usage in a notebook:

    %load_ext schemadex
    %schemadex --url sqlite:///tmp/demo.sqlite list-tables
    %schemadex --url sqlite:///tmp/demo.sqlite describe customers
    %schemadex --url sqlite:///tmp/demo.sqlite resolve orders customer_idd

Or the line-magic shorthand once a default URL is set:

    %schemadex_url sqlite:///tmp/demo.sqlite
    %schemadex describe orders
"""

from __future__ import annotations

import shlex
from typing import Any

from schemadex import SchemaCache


_DEFAULT_URL: str | None = None
_CACHE: SchemaCache | None = None


def _get_cache(url: str | None) -> SchemaCache:
    global _CACHE, _DEFAULT_URL
    chosen = url or _DEFAULT_URL
    if not chosen:
        raise RuntimeError("no schemadex URL set; pass --url or run %schemadex_url first")
    if _CACHE is None or chosen != _DEFAULT_URL:
        _CACHE = SchemaCache.from_url(chosen)
        _DEFAULT_URL = chosen
    return _CACHE


def _schemadex_url(line: str) -> str:
    global _DEFAULT_URL
    _DEFAULT_URL = line.strip()
    return f"schemadex URL set to {_DEFAULT_URL}"


def _schemadex(line: str) -> Any:
    """Dispatcher: parse `[--url URL] <verb> [args...]` from the line."""
    parts = shlex.split(line)
    url: str | None = None
    if parts and parts[0] == "--url":
        url = parts[1]
        parts = parts[2:]
    if not parts:
        return "usage: %schemadex [--url URL] list-tables | describe <table> | resolve <table> <candidate>"
    verb, *rest = parts
    cache = _get_cache(url)
    if verb == "list-tables":
        return cache.list_tables()
    if verb == "describe":
        if not rest:
            return cache.describe_for_agent(max_tokens=2048)[0]
        # describe one table only
        return cache.get_table(rest[0])
    if verb == "resolve":
        if len(rest) < 2:
            return "usage: resolve <table> <candidate>"
        return cache.resolve(rest[0], rest[1])
    return f"unknown verb: {verb}"


def load_ipython_extension(ipython: Any) -> None:
    """IPython entry point. Called by `%load_ext schemadex`."""
    ipython.register_magic_function(_schemadex_url, "line", "schemadex_url")
    ipython.register_magic_function(_schemadex, "line", "schemadex")
