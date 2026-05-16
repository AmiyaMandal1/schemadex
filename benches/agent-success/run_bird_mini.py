"""BIRD-mini wiring stub (v0.3 item 4).

Skeleton for a frontier-model SQL-generation benchmark against a small
subset of BIRD. The point of landing this now is so that the CI matrix
has a typecheck-clean entry point: when ``ANTHROPIC_API_KEY`` or
``OPENAI_API_KEY`` is present in the environment, the harness runs end
to end; otherwise it prints a friendly skip message and exits 0.

What it does when an API key is set:

1. Load a BIRD-mini corpus (JSON list of
   ``{"id", "question", "db_url", ...}``).
2. For each record, build a ``schemadex`` ``SchemaCache`` from
   ``db_url``, call ``describe_for_agent(hint=question)``, and prompt the
   model to emit a single SQL query.
3. Log ``{"id", "question", "sql", "latency_ms"}`` per record to
   ``out/bird_mini_<provider>.jsonl``. **It does not execute the SQL** —
   correctness scoring happens out of band.

Stdlib HTTP only — no new pip deps.

Run:
    # no-op (default; no API key in env):
    python benches/agent-success/run_bird_mini.py --corpus /dev/null

    # real run:
    export ANTHROPIC_API_KEY=sk-ant-...
    python benches/agent-success/run_bird_mini.py \\
        --corpus benches/agent-success/bird_mini.json \\
        --provider anthropic --model claude-sonnet-4-6 --n 50
"""

from __future__ import annotations

import argparse
import json
import os
import pathlib
import re
import sys
import time
import urllib.request
from typing import Any

from schemadex import SchemaCache

HERE = pathlib.Path(__file__).parent
OUT = HERE / "out"

ANTHROPIC_URL = "https://api.anthropic.com/v1/messages"
OPENAI_URL = "https://api.openai.com/v1/chat/completions"


# ---------------------------------------------------------------------------
# Provider clients.
# ---------------------------------------------------------------------------


def _http_post(url: str, headers: dict[str, str], body: dict[str, Any]) -> dict[str, Any]:
    data = json.dumps(body).encode()
    req = urllib.request.Request(url, data=data, headers=headers, method="POST")
    with urllib.request.urlopen(req, timeout=120.0) as resp:
        return json.load(resp)


def call_anthropic(model: str, prompt: str, api_key: str) -> str:
    payload = {
        "model": model,
        "max_tokens": 1024,
        "messages": [{"role": "user", "content": prompt}],
    }
    headers = {
        "Content-Type": "application/json",
        "x-api-key": api_key,
        "anthropic-version": "2023-06-01",
    }
    resp = _http_post(ANTHROPIC_URL, headers, payload)
    parts = resp.get("content", [])
    out: list[str] = []
    for p in parts:
        if isinstance(p, dict) and p.get("type") == "text":
            out.append(str(p.get("text", "")))
    return "".join(out)


def call_openai(model: str, prompt: str, api_key: str) -> str:
    payload = {
        "model": model,
        "messages": [{"role": "user", "content": prompt}],
        "max_tokens": 1024,
    }
    headers = {
        "Content-Type": "application/json",
        "Authorization": f"Bearer {api_key}",
    }
    resp = _http_post(OPENAI_URL, headers, payload)
    choices = resp.get("choices", [])
    if not choices:
        return ""
    return str(choices[0].get("message", {}).get("content", ""))


# ---------------------------------------------------------------------------
# SQL extraction.
# ---------------------------------------------------------------------------


SQL_FENCE_RE = re.compile(r"```(?:sql)?\s*(.+?)```", re.DOTALL | re.IGNORECASE)


def extract_sql(text: str) -> str:
    """Pull the first SQL statement out of the model's reply."""
    m = SQL_FENCE_RE.search(text)
    if m:
        return m.group(1).strip()
    # Fall back to the first ``SELECT``/``WITH``-prefixed chunk.
    m2 = re.search(r"\b(?:SELECT|WITH)\b.+?;", text, re.DOTALL | re.IGNORECASE)
    if m2:
        return m2.group(0).strip()
    return text.strip()


# ---------------------------------------------------------------------------
# Prompt.
# ---------------------------------------------------------------------------


def make_prompt(schema_description: str, question: str) -> str:
    return (
        "You are a SQL expert. Given the schema description below and a "
        "natural-language question, respond with ONLY a single SQL "
        "statement that answers the question. Wrap the SQL in a ```sql "
        "fenced code block. Do not add prose.\n\n"
        "Schema:\n"
        f"{schema_description}\n\n"
        f"Question: {question}"
    )


# ---------------------------------------------------------------------------
# Main runner.
# ---------------------------------------------------------------------------


def _resolve_api_key(provider: str) -> tuple[str, str] | None:
    """Return (provider, api_key) or None if no key is set."""
    anthropic_key = os.environ.get("ANTHROPIC_API_KEY", "").strip()
    openai_key = os.environ.get("OPENAI_API_KEY", "").strip()
    if provider == "anthropic" and anthropic_key:
        return ("anthropic", anthropic_key)
    if provider == "openai" and openai_key:
        return ("openai", openai_key)
    # Allow auto-fallback if the requested provider's key isn't set but
    # the other is — we still need *some* key to run.
    if anthropic_key:
        return ("anthropic", anthropic_key)
    if openai_key:
        return ("openai", openai_key)
    return None


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--corpus", required=True, help="Path to BIRD-mini JSON corpus")
    ap.add_argument("--model", default="claude-sonnet-4-6")
    ap.add_argument("--provider", choices=("anthropic", "openai"), default="anthropic")
    ap.add_argument("--n", type=int, default=50)
    args = ap.parse_args(argv)

    resolved = _resolve_api_key(args.provider)
    if resolved is None:
        print("Skipping: no API key in env")
        return 0

    provider, api_key = resolved
    corpus_path = pathlib.Path(args.corpus)
    if not corpus_path.exists():
        print(f"missing corpus: {corpus_path}", file=sys.stderr)
        return 1

    corpus = json.loads(corpus_path.read_text())
    if args.n > 0:
        corpus = corpus[: args.n]

    OUT.mkdir(exist_ok=True)
    out_path = OUT / f"bird_mini_{provider}.jsonl"
    with out_path.open("w") as fh:
        for rec in corpus:
            db_url = rec.get("db_url")
            question = rec.get("question", "")
            rec_id = rec.get("id")
            if not db_url:
                # Allow records that ship a literal schema string instead.
                schema_description = rec.get("schema_description", "")
            else:
                cache = SchemaCache.from_url(db_url)
                schema_description, _ = cache.describe_for_agent(
                    max_tokens=2048, hint=question, include_samples=True
                )
            prompt = make_prompt(schema_description, question)

            t0 = time.perf_counter()
            try:
                if provider == "anthropic":
                    response = call_anthropic(args.model, prompt, api_key)
                else:
                    response = call_openai(args.model, prompt, api_key)
            except Exception as exc:  # noqa: BLE001
                response = f"ERROR: {exc}"
            latency_ms = (time.perf_counter() - t0) * 1000.0

            sql = extract_sql(response)
            fh.write(
                json.dumps(
                    {
                        "id": rec_id,
                        "question": question,
                        "sql": sql,
                        "latency_ms": latency_ms,
                    }
                )
                + "\n"
            )

    print(f"wrote {out_path} ({len(corpus)} records, provider={provider}, model={args.model})")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
