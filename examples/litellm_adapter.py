"""LiteLLM adapter for schemadex.

Builds a system + user message pair suitable for `litellm.completion(...)`.
Lets users call any of LiteLLM's 100+ supported model providers without
re-implementing schema-prompt construction.

Requires:

    pip install "schemadex[litellm]"
"""

from __future__ import annotations

from typing import Any

import litellm

from schemadex import SchemaCache


def schemadex_messages(
    cache: SchemaCache,
    question: str,
    *,
    max_tokens: int = 2048,
    include_samples: bool = True,
    extra_system: str | None = None,
) -> list[dict[str, str]]:
    """Return a list of chat messages with the schemadex schema as the system context."""
    text, _tokens = cache.describe_for_agent(
        max_tokens=max_tokens,
        hint=question,
        include_samples=include_samples,
    )
    system_parts: list[str] = []
    if extra_system:
        system_parts.append(extra_system)
    system_parts.append(
        "You are a SQL agent. Use only the schema below; do not invent tables or columns.\n\n"
        + text
    )
    return [
        {"role": "system", "content": "\n\n".join(system_parts)},
        {"role": "user", "content": question},
    ]


def schemadex_completion(
    cache: SchemaCache,
    question: str,
    *,
    model: str = "ollama/qwen2.5-coder:3b",
    max_tokens: int = 2048,
    include_samples: bool = True,
    **litellm_kwargs: Any,
) -> Any:
    """Convenience: build messages + call litellm.completion in one shot."""
    messages = schemadex_messages(
        cache,
        question,
        max_tokens=max_tokens,
        include_samples=include_samples,
    )
    return litellm.completion(model=model, messages=messages, **litellm_kwargs)
