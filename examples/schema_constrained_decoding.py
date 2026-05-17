"""Schema-constrained decoding example.

Two flavors:

1. vLLM grammar — produce a regex that matches every valid column /
   table name. Pass as ``extra_body={"guided_regex": pattern}`` to
   vLLM's OpenAI-compatible ``/chat/completions``.

2. OpenAI logit_bias — pre-tokenize every valid identifier and bias the
   model toward those tokens (requires ``tiktoken``).

Run-mode: ``python examples/schema_constrained_decoding.py
--url sqlite:///tmp/demo.sqlite`` prints the regex / bias dict;
doesn't make any LLM call.
"""

from __future__ import annotations

import argparse
import re
import sys

from schemadex import SchemaCache


def build_regex(cache: SchemaCache) -> str:
    """Return a regex anchored to identifier boundaries covering every
    table and column name the cache knows.
    """
    names: set[str] = set(cache.list_tables())
    for t in cache.list_tables():
        tbl = cache.get_table(t) or {}
        for c in tbl.get("columns", []):
            names.add(c["name"])
    escaped = sorted(re.escape(n) for n in names)
    return r"\b(?:" + "|".join(escaped) + r")\b"


def build_logit_bias(cache: SchemaCache, tokenizer) -> dict[int, int]:
    """Pre-tokenize each identifier; bias the model toward those token ids.

    ``tokenizer.encode(text) -> list[int]`` is the contract; works with
    ``tiktoken.Encoding`` directly.
    """
    bias: dict[int, int] = {}
    for t in cache.list_tables():
        for tok in tokenizer.encode(t):
            bias[tok] = 5
        tbl = cache.get_table(t) or {}
        for c in tbl.get("columns", []):
            for tok in tokenizer.encode(c["name"]):
                bias[tok] = 3
    return bias


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--url", required=True)
    ap.add_argument("--what", choices=["regex", "bias"], default="regex")
    args = ap.parse_args()
    cache = SchemaCache.from_url(args.url)
    if args.what == "regex":
        print(build_regex(cache))
        return 0
    try:
        import tiktoken
    except ImportError:
        print("tiktoken not installed; pip install tiktoken", file=sys.stderr)
        return 1
    tk = tiktoken.get_encoding("cl100k_base")
    bias = build_logit_bias(cache, tk)
    sample = dict(list(bias.items())[:20])
    print(f"total tokens biased: {len(bias)}; preview: {sample}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
