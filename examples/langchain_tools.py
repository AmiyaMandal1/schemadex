"""LangChain `Tool` adapters for schemadex.

Drop these into a LangChain agent to give it a sane schema-discovery surface
instead of letting it free-text-guess column names.

Requires:

    pip install "schemadex[langchain]"
"""

from __future__ import annotations

from typing import Any

from langchain_core.tools import Tool

from schemadex import SchemaCache


def make_schema_tools(cache: SchemaCache) -> list[Tool]:
    """Return a list of LangChain tools backed by an already-populated cache."""

    def _list_tables(_: str) -> str:
        return "\n".join(cache.list_tables())

    def _describe(query: str) -> str:
        prompt, _tokens = cache.describe_for_agent(max_tokens=2048, hint=query or None)
        return prompt

    def _resolve(spec: str) -> str:
        try:
            table, candidate = (s.strip() for s in spec.split(",", 1))
        except ValueError:
            return "expected 'table_name, candidate_column'"
        r = cache.resolve(table, candidate)
        alts = ", ".join(f"{n} ({c:.2f})" for n, c in r.alternatives)
        return f"matched={r.matched} confidence={r.confidence:.2f} alternatives=[{alts}]"

    return [
        Tool(
            name="schemadex_list_tables",
            description="List every table in the connected database.",
            func=_list_tables,
        ),
        Tool(
            name="schemadex_describe_for_agent",
            description=(
                "Return a token-budgeted schema description. The input is a free-text "
                "hint (e.g. 'orders by region') that biases table ranking."
            ),
            func=_describe,
        ),
        Tool(
            name="schemadex_resolve_column",
            description=(
                "Fuzzy-resolve a candidate column name on a table. "
                "Input format: 'table_name, candidate_column'."
            ),
            func=_resolve,
        ),
    ]
