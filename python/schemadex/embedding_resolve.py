"""Embedding-based fallback for column resolution.

When :func:`schemadex.resolve` (a Jaro-Winkler fuzzy match) returns a
low-confidence answer, this module re-ranks candidate columns using cosine
similarity of embeddings produced by a local Ollama model.

The embedding call is intentionally kept out of the hot Rust path: it's a
slow HTTP round-trip that's strictly opt-in, and stdlib ``urllib`` is more
than enough.

Performance: an on-disk embedding cache (``embeddings.json.zst``, written
next to ``database.json.zst``) is consulted before any HTTP traffic. When a
hit covers the candidate column pool we skip Ollama entirely and rank with
NumPy (if available) or pure-Python ``math.sqrt``. On a miss we fall back to
Ollama and persist the fresh embeddings so the next call is fast.

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

try:  # NumPy is optional — we only use it for the inner product hot loop.
    import numpy as _np  # type: ignore
except Exception:  # pragma: no cover - import-time defensive
    _np = None  # type: ignore

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


def _cosine_py(a: list[float], b: list[float]) -> float:
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


def _cosine(a: list[float], b: list[float]) -> float:
    """Cosine similarity. Uses NumPy when available, falls back to stdlib."""
    if _np is None:
        return _cosine_py(a, b)
    va = _np.asarray(a, dtype=_np.float32)
    vb = _np.asarray(b, dtype=_np.float32)
    if va.shape != vb.shape or va.size == 0:
        return 0.0
    denom = float(_np.linalg.norm(va) * _np.linalg.norm(vb))
    if denom == 0.0:
        return 0.0
    return float(_np.dot(va, vb) / denom)


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


def _load_persisted_index(cache: Any, model: str) -> dict[str, dict[str, list[float]]] | None:
    """Pull the on-disk embedding index off ``cache`` when present and
    keyed to ``model``. Returns the ``by_column`` mapping or ``None`` when
    the cache is empty, stale (different model), or the binding is missing.
    """
    loader = getattr(cache, "load_embeddings", None)
    if loader is None:
        return None
    try:
        idx = loader()
    except Exception as exc:  # pragma: no cover - defensive
        _warn(f"load_embeddings raised {exc!r}; falling back to HTTP")
        return None
    if not idx:
        return None
    cached_model = idx.get("model") if isinstance(idx, dict) else None
    if cached_model and cached_model != model:
        # Stale index — wrong model. Caller will refresh.
        return None
    by_col = idx.get("by_column") if isinstance(idx, dict) else None
    if not isinstance(by_col, dict):
        return None
    return by_col


def _persist_index(
    cache: Any,
    *,
    model: str,
    table_key: str,
    columns: dict[str, list[float]],
    existing: dict[str, dict[str, list[float]]] | None,
) -> None:
    """Merge ``columns`` into the persisted index under ``table_key`` and
    write it back. Failures are logged and swallowed — the embedding rank
    succeeded; persistence is a best-effort speedup."""
    storer = getattr(cache, "store_embeddings", None)
    if storer is None:
        return
    by_col: dict[str, dict[str, list[float]]] = dict(existing or {})
    table_entry: dict[str, list[float]] = dict(by_col.get(table_key) or {})
    table_entry.update(columns)
    by_col[table_key] = table_entry
    dim = 0
    for tbl_cols in by_col.values():
        for v in tbl_cols.values():
            dim = len(v)
            break
        if dim:
            break
    try:
        storer({"model": model, "dim": dim, "by_column": by_col})
    except Exception as exc:  # pragma: no cover - defensive
        _warn(f"store_embeddings raised {exc!r}; cache not updated")


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
       column on the table). For each column, prefer a persisted embedding
       from ``cache.load_embeddings()``; only call Ollama for cache misses.
       Rank by cosine similarity to the embedding of ``candidate``.
    4. Persist any freshly-computed embeddings back to the cache so the
       next call short-circuits the HTTP path entirely.

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

    table_key: str | None = None
    if table_info:
        # Prefer the cache's own qualified name so the persisted index key
        # matches what Rust writes (e.g. ``public.users`` not ``users``).
        schema = table_info.get("schema") if isinstance(table_info, dict) else None
        name = table_info.get("name") if isinstance(table_info, dict) else None
        if name:
            table_key = f"{schema}.{name}" if schema else name
        else:
            table_key = table
        for col in table_info.get("columns") or []:
            col_name = (
                col.get("name") if isinstance(col, dict) else getattr(col, "name", None)
            )
            _add(col_name)
    else:
        table_key = table

    if not pool:
        return lexical_result

    # Step 1: harvest cached vectors for the pool, so we only POST what's
    # missing.
    persisted = _load_persisted_index(cache, model) or {}
    table_cached = persisted.get(table_key or "", {}) if persisted else {}
    vectors: dict[str, list[float]] = {}
    newly_fetched: dict[str, list[float]] = {}
    for name in pool:
        cached_vec = table_cached.get(name)
        if isinstance(cached_vec, list) and cached_vec:
            vectors[name] = [float(x) for x in cached_vec]

    # Step 2: embed the input candidate. Always fresh — it isn't a column
    # name, so the index doesn't carry it.
    query_vec = _embed(
        candidate, ollama_url=ollama_url, model=model, timeout=timeout
    )
    if query_vec is None:
        return lexical_result

    # Step 3: fetch any pool members we don't have cached yet.
    for name in pool:
        if name in vectors:
            continue
        vec = _embed(name, ollama_url=ollama_url, model=model, timeout=timeout)
        if vec is None:
            # Any failure mid-pool aborts the rerank — partial results
            # would be misleading.
            return lexical_result
        vectors[name] = vec
        newly_fetched[name] = vec

    # Step 4: rank.
    scored = [(name, _cosine(query_vec, vectors[name])) for name in pool]
    scored.sort(key=lambda x: x[1], reverse=True)
    if not scored:
        return lexical_result

    # Step 5: best-effort persist freshly-fetched vectors so future calls
    # short-circuit Ollama. We never write if there was nothing new.
    if newly_fetched and table_key:
        _persist_index(
            cache,
            model=model,
            table_key=table_key,
            columns=newly_fetched,
            existing=persisted,
        )

    best_name, best_score = scored[0]
    alternatives = scored[1:4]

    return ResolveResult(
        matched=best_name,
        confidence=float(best_score),
        alternatives=[(n, float(s)) for n, s in alternatives],
    )
