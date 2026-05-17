"""Anthropic API adapter for schemadex.

Builds a `messages.create(...)` payload with the schema description in
a cache-controlled system block. Cuts per-turn cost ~90% on Claude
when the cache hits.

Requires:

    pip install "schemadex[anthropic]" anthropic
"""

from __future__ import annotations
from typing import Any
from schemadex import SchemaCache


def schemadex_anthropic_messages(
    cache: SchemaCache,
    question: str,
    *,
    max_tokens: int = 2048,
    hint: str | None = None,
    include_samples: bool = True,
    include_foreign_keys: bool = True,
    include_examples: bool = False,
) -> dict[str, Any]:
    """Return a kwargs dict suitable for `anthropic.Anthropic().messages.create(**kwargs)`."""
    text, _ = cache.describe_for_agent(
        max_tokens=max_tokens,
        hint=hint or question,
        include_samples=include_samples,
        include_foreign_keys=include_foreign_keys,
        include_examples=include_examples,
    )
    return {
        "system": [
            {
                "type": "text",
                "text": (
                    "You are a SQL agent. Use only the schema below; do "
                    "not invent tables or columns.\n\n" + text
                ),
                "cache_control": {"type": "ephemeral"},
            }
        ],
        "messages": [{"role": "user", "content": question}],
    }
