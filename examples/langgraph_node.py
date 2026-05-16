"""LangGraph node adapter for schemadex.

Use this as a node in a LangGraph that needs to inject a schema description
into the agent's state before the planner runs.

Requires:

    pip install "schemadex[langgraph]"
"""

from __future__ import annotations

from typing import TypedDict

from schemadex import SchemaCache


class AgentState(TypedDict, total=False):
    question: str
    schema_prompt: str
    schema_tokens: int


def schema_node(cache: SchemaCache, *, max_tokens: int = 2048):
    """Return a LangGraph-compatible callable that adds `schema_prompt` to state.

    Example::

        from langgraph.graph import StateGraph

        graph = StateGraph(AgentState)
        graph.add_node("schema", schema_node(cache))
        graph.set_entry_point("schema")
    """

    def _node(state: AgentState) -> AgentState:
        hint = state.get("question")
        prompt, tokens = cache.describe_for_agent(max_tokens=max_tokens, hint=hint)
        return {**state, "schema_prompt": prompt, "schema_tokens": tokens}

    return _node
