"""Anthropic adapter smoke test.

The example builds a `messages.create(...)` kwargs dict around the
schemadex describe output. We verify the dict shape, the cache_control
markers, and the user message — no live API call.
"""

from __future__ import annotations

import sqlite3
import sys
from pathlib import Path

import pytest

schemadex = pytest.importorskip("schemadex")


# Wire the examples dir onto sys.path so the adapter module can be imported
# directly without being installed as a package.
EXAMPLES_DIR = Path(__file__).resolve().parent.parent / "examples"
if str(EXAMPLES_DIR) not in sys.path:
    sys.path.insert(0, str(EXAMPLES_DIR))


def seed(path: Path) -> None:
    conn = sqlite3.connect(path)
    conn.executescript(
        """
        CREATE TABLE customers (id INTEGER PRIMARY KEY, email TEXT NOT NULL);
        INSERT INTO customers (email) VALUES ('a@x.com'), ('b@x.com');
        """
    )
    conn.commit()
    conn.close()


def test_anthropic_kwargs_shape(tmp_path: Path) -> None:
    from anthropic_messages import schemadex_anthropic_messages  # noqa: E402

    db = tmp_path / "demo.sqlite"
    seed(db)
    url = f"sqlite://{db}"
    cache = schemadex.SchemaCache.from_url(url, cache_dir=str(tmp_path / "cache"))

    kwargs = schemadex_anthropic_messages(
        cache, "list customer emails", max_tokens=512
    )

    # Top-level shape: a `system` list and a `messages` list.
    assert isinstance(kwargs["system"], list)
    assert isinstance(kwargs["messages"], list)

    system_block = kwargs["system"][0]
    assert system_block["type"] == "text"
    assert "schema" in system_block["text"].lower() or "customers" in system_block["text"]
    # Prompt-caching marker present and tagged ephemeral.
    assert system_block["cache_control"] == {"type": "ephemeral"}

    user_msg = kwargs["messages"][0]
    assert user_msg["role"] == "user"
    assert user_msg["content"] == "list customer emails"
