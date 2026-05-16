"""Embedding-based fallback for column resolution.

When :func:`schemadex.resolve` (a Jaro-Winkler fuzzy match) returns a
low-confidence answer, this module re-ranks candidate columns using cosine
similarity of embeddings produced by a local Ollama model.

The embedding call is intentionally kept out of the hot Rust path: it's a
slow HTTP round-trip that's strictly opt-in, and stdlib ``urllib`` is more
than enough.

Failure handling: if ollama is unreachable, returns a non-200, or otherwise
misbehaves, we log a warning to stderr and return the original lexical
result unchanged. We never raise.
"""

from __future__ import annotations

import json
import math
import sys
import urllib.error
import urllib.request
from dataclasses import dataclass, field
from typing import Any

__all__ = ["ResolveResult", "resolve_with_embedding"]


@dataclass
class ResolveResult:
    """Mirror of the native ``ResolveResult`` shape.

    We return our own dataclass instead of constructing the PyO3 type so
    this module stays pure-Python and trivially importable in any
    environment that has the wheel installed.
    """

    matched: str | None
    confidence: float
    alternatives: list[tuple[str, float]] = field(default_factory=list)


def _warn(msg: str) -> None:
    print(f"[schemadex.embedding_resolve] {msg}", file=sys.stderr)


def _embed(
    text: str, *, ollama_url: str, model: str, timeout: float
) -> list[float] | None:
    """POST to ``/api/embeddings`` and return the embedding vector.

    Returns ``None`` on any error (network, HTTP, JSON parse, missing key).
    """
    url = ollama_url.rstrip("/") + "/api/embeddings"
    body = json.dumps({"model": model, "prompt": text}).encode("utf-8")
    req = urllib.request.Request(
        url,
        data=body,
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            raw = resp.read()
    except (urllib.error.URLError, TimeoutError, OSError) as exc:
        _warn(f"embedding request failed for {text!r}: {exc}")
        return None

    try:
        payload = json.loads(raw)
    except json.JSONDecodeError as exc:
        _warn(f"embedding response was not JSON: {exc}")
        return None

    vec = payload.get("embedding")
    if not isinstance(vec, list) or not vec:
        # Ollama returns {"error": "..."} when the model isn't loaded etc.
        err = payload.get("error")
        if err:
            _warn(f"ollama error: {err}")
        else:
            _warn(f"embedding payload missing 'embedding' key: {payload!r}")
        return None
    return [float(x) for x in vec]


def _cosine(a: list[float], b: list[float]) -> float:
    if len(a) != len(b) or not a:
        return 0.0
    dot = 0.0
    na = 0.0
    nb = 0.0
    for x, y in zip(a, b):
        dot += x * y
        na += x * x
        nb += y * y
    denom = math.sqrt(na) * math.sqrt(nb)
    if denom == 0.0:
        return 0.0
    return dot / denom


def _to_result(obj: Any) -> ResolveResult:
    """Convert a native ResolveResult (or anything with the same attrs)
    into our dataclass shape so the return type is consistent."""
    return ResolveResult(
        matched=getattr(obj, "matched", None),
        confidence=float(getattr(obj, "confidence", 0.0)),
        alternatives=[
            (str(name), float(score))
            for name, score in getattr(obj, "alternatives", []) or []
        ],
    )


def resolve_with_embedding(
    cache: Any,
    table: str,
    candidate: str,
    *,
    threshold: float = 0.85,
    ollama_url: str = "http://localhost:11434",
    model: str = "nomic-embed-text-v2-moe",
    timeout: float = 5.0,
) -> ResolveResult:
    """Resolve ``candidate`` against ``table`` with an embedding fallback.

    1. Call the lexical resolver via ``cache.resolve``.
    2. If its confidence is >= ``threshold``, return it unchanged.
    3. Otherwise, build a candidate pool of (matched + alternatives + every
       column on the table), embed both sides via Ollama, and pick the
       column whose embedding has the highest cosine similarity to the
       input ``candidate``.

    The returned ``confidence`` for an embedding-reranked winner is the
    cosine similarity itself (range roughly [-1, 1], but in practice [0, 1]
    for these models).

    If Ollama is unreachable or any embedding call fails, the original
    lexical result is returned and a warning is printed to stderr.
    """
    lexical = cache.resolve(table, candidate)
    lexical_result = _to_result(lexical)

    if lexical_result.confidence >= threshold:
        return lexical_result

    # Build candidate pool: matched (if any) + alternatives + all columns
    # on the table. De-dup while preserving order.
    pool: list[str] = []
    seen: set[str] = set()

    def _add(name: str | None) -> None:
        if not name:
            return
        if name in seen:
            return
        seen.add(name)
        pool.append(name)

    _add(lexical_result.matched)
    for alt_name, _score in lexical_result.alternatives:
        _add(alt_name)

    table_info = None
    try:
        table_info = cache.get_table(table)
    except Exception as exc:  # pragma: no cover - defensive
        _warn(f"cache.get_table({table!r}) raised {exc!r}; using lexical pool only")

    if table_info:
        for col in table_info.get("columns") or []:
            name = col.get("name") if isinstance(col, dict) else getattr(col, "name", None)
            _add(name)

    if not pool:
        return lexical_result

    # Embed the input candidate first; if that fails, bail out.
    query_vec = _embed(
        candidate, ollama_url=ollama_url, model=model, timeout=timeout
    )
    if query_vec is None:
        return lexical_result

    scored: list[tuple[str, float]] = []
    for name in pool:
        vec = _embed(name, ollama_url=ollama_url, model=model, timeout=timeout)
        if vec is None:
            # Any failure mid-pool aborts the rerank — partial results
            # would be misleading.
            return lexical_result
        scored.append((name, _cosine(query_vec, vec)))

    if not scored:
        return lexical_result

    scored.sort(key=lambda x: x[1], reverse=True)
    best_name, best_score = scored[0]
    alternatives = scored[1:4]

    return ResolveResult(
        matched=best_name,
        confidence=float(best_score),
        alternatives=[(n, float(s)) for n, s in alternatives],
    )
