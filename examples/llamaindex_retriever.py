"""LlamaIndex retriever adapter for schemadex.

Wraps `SchemaCache.describe_for_agent` as a LlamaIndex `BaseRetriever` so
the LlamaIndex agent stack can use it the same way it uses a vector store.

Requires:

    pip install "schemadex[llamaindex]"
"""

from __future__ import annotations

from typing import Any

from llama_index.core.retrievers import BaseRetriever
from llama_index.core.schema import NodeWithScore, TextNode

from schemadex import SchemaCache


class SchemaIndexRetriever(BaseRetriever):
    """Returns a single text node containing the token-budgeted schema
    description biased toward the user's query."""

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

    def _retrieve(self, query_bundle: Any) -> list[NodeWithScore]:
        question = query_bundle.query_str if hasattr(query_bundle, "query_str") else str(query_bundle)
        text, tokens = self._cache.describe_for_agent(
            max_tokens=self._max_tokens,
            hint=question,
            include_samples=self._include_samples,
        )
        node = TextNode(text=text, metadata={"schema_tokens": tokens})
        return [NodeWithScore(node=node, score=1.0)]
