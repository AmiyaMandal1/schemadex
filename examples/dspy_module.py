"""DSPy adapter for schemadex.

Exposes schemadex as a DSPy `Module` that emits the schema description
plus a SQL plan slot. Drop it into a DSPy pipeline before the SQL-writing
module so the writer sees ranked schema + sentinel flags.

Requires:

    pip install "schemadex[dspy]"
"""

from __future__ import annotations

from typing import Any

import dspy

from schemadex import SchemaCache


class SchemadexContext(dspy.Module):
    def __init__(
        self,
        cache: SchemaCache,
        *,
        max_tokens: int = 2048,
        include_samples: bool = True,
    ) -> None:
        super().__init__()
        self._cache = cache
        self._max_tokens = max_tokens
        self._include_samples = include_samples

    def forward(self, question: str) -> dspy.Prediction:
        text, tokens = self._cache.describe_for_agent(
            max_tokens=self._max_tokens,
            hint=question,
            include_samples=self._include_samples,
        )
        return dspy.Prediction(
            schema=text,
            schema_tokens=tokens,
            question=question,
        )
