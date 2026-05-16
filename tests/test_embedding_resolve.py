"""Integration test for :func:`schemadex.resolve_with_embedding`.

Skipped automatically when Ollama is not reachable on localhost:11434, or
when the required embedding model isn't loaded. When it does run, it spins
up a tiny sqlite database whose column names ("status", "body") are
*semantically* close to the agent's guesses ("state", "review_body") but
lexically far enough that the Jaro-Winkler resolver falls under the 0.85
threshold and the embedding reranker takes over.
"""

from __future__ import annotations

import json
import sqlite3
import urllib.error
import urllib.request
from pathlib import Path

import pytest

schemadex = pytest.importorskip("schemadex")

OLLAMA_URL = "http://localhost:11434"
MODEL = "nomic-embed-text-v2-moe"


def _ollama_ready() -> tuple[bool, str]:
    """Return ``(ok, reason)``.

    Verifies Ollama answers ``/api/tags`` *and* can produce an embedding
    with the configured model. Both checks have to pass — the model might
    be in the tag list but unloaded, or the server might be up but the
    model uninstalled.
    """
    try:
        with urllib.request.urlopen(f"{OLLAMA_URL}/api/tags", timeout=2.0) as r:
            tags_raw = r.read()
    except (urllib.error.URLError, TimeoutError, OSError) as exc:
        return False, f"ollama not reachable at {OLLAMA_URL}: {exc}"

    try:
        tags = json.loads(tags_raw)
    except json.JSONDecodeError as exc:
        return False, f"ollama /api/tags returned non-JSON: {exc}"

    names = {m.get("name", "") for m in tags.get("models", [])}
    # Ollama tags include the tag suffix (``:latest``); a bare model name
    # might match either form.
    if not any(name == MODEL or name.startswith(f"{MODEL}:") for name in names):
        return False, f"embedding model {MODEL!r} not installed on ollama"

    # Final probe: actually generate an embedding. Catches "model is
    # listed but failed to load" type problems.
    body = json.dumps({"model": MODEL, "prompt": "ping"}).encode("utf-8")
    req = urllib.request.Request(
        f"{OLLAMA_URL}/api/embeddings",
        data=body,
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    try:
        with urllib.request.urlopen(req, timeout=15.0) as r:
            payload = json.loads(r.read())
    except (urllib.error.URLError, TimeoutError, OSError) as exc:
        return False, f"embedding probe failed: {exc}"
    except json.JSONDecodeError as exc:
        return False, f"embedding probe returned non-JSON: {exc}"

    if not isinstance(payload.get("embedding"), list) or not payload["embedding"]:
        return False, f"embedding probe returned no vector: {payload!r}"
    return True, ""


_ok, _skip_reason = _ollama_ready()
pytestmark = pytest.mark.skipif(not _ok, reason=_skip_reason or "ollama unavailable")


def _seed(path: Path) -> None:
    """Create a ``users`` table with column names lexically distant from
    the test inputs but semantically obvious to an embedding model."""
    conn = sqlite3.connect(path)
    conn.executescript(
        """
        CREATE TABLE users (
            id INTEGER PRIMARY KEY,
            status TEXT NOT NULL,
            body TEXT
        );
        INSERT INTO users (status, body) VALUES
            ('active', 'first user'),
            ('inactive', 'second user');
        """
    )
    conn.commit()
    conn.close()


@pytest.fixture()
def cache(tmp_path: Path):
    db = tmp_path / "demo.sqlite"
    _seed(db)
    return schemadex.SchemaCache.from_url(
        f"sqlite://{db}", cache_dir=str(tmp_path / "cache")
    )


def test_state_resolves_to_status(cache) -> None:
    """``state`` should resolve to ``status``.

    Jaro-Winkler happens to land above 0.85 here (``state``/``status``
    share a long prefix), so the embedding fallback isn't triggered in
    this case — but the contract is "you get ``status`` back", which holds
    regardless of which path produced it.
    """
    result = schemadex.resolve_with_embedding(cache, "users", "state")
    assert result.matched == "status", (
        f"expected 'status', got matched={result.matched!r} "
        f"confidence={result.confidence:.3f} alts={result.alternatives}"
    )
    assert 0.0 <= result.confidence <= 1.0


def test_review_body_reranks_to_body(cache) -> None:
    """``review_body`` is lexically far from ``body`` (Jaro-Winkler scores
    it well under 0.85) so the embedding fallback is what picks ``body``."""
    lex = cache.resolve("users", "review_body")
    result = schemadex.resolve_with_embedding(cache, "users", "review_body")
    assert result.matched == "body", (
        f"expected 'body', got matched={result.matched!r} "
        f"confidence={result.confidence:.3f} alts={result.alternatives} "
        f"(lexical={lex.matched!r}@{lex.confidence:.3f})"
    )
    assert 0.0 <= result.confidence <= 1.0


def test_high_confidence_lexical_is_returned_untouched(cache) -> None:
    """When the lexical resolver already clears the threshold the function
    must short-circuit and return the native result without hitting ollama.

    We point at a bogus URL so that *any* embedding call would error out;
    if the fast path is skipped this test will explode instead of silently
    falling back."""
    result = schemadex.resolve_with_embedding(
        cache,
        "users",
        "status",  # exact match, confidence 1.0
        ollama_url="http://127.0.0.1:1",  # guaranteed connection refused
        timeout=1.0,
    )
    assert result.matched == "status"
    assert result.confidence >= 0.99
