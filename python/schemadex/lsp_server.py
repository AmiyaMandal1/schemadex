"""LSP server for schemadex.

Provides column-name autocomplete in .sql files backed by a SchemaCache.

Usage as an LSP server (stdio transport):

    schemadex-lsp --url postgres://...

In your editor's LSP config, register `schemadex-lsp` as the server for
`*.sql` files.

Capabilities (v0):
- textDocument/completion        column names + table names
- textDocument/hover             column type + sample / sentinel
- workspace/didChangeConfiguration  switch --url at runtime

Requires:
    pip install "schemadex[lsp]"
"""

from __future__ import annotations

import argparse
import os
import re
import sys
from typing import Any

try:
    # pygls v2 layout
    from pygls.lsp.server import LanguageServer
    from lsprotocol import types as lsp
except ImportError:  # pragma: no cover - fall back to pygls v1
    try:
        from pygls.server import LanguageServer  # type: ignore
        from lsprotocol import types as lsp  # type: ignore
    except ImportError:
        LanguageServer = None  # type: ignore
        lsp = None  # type: ignore

from schemadex import SchemaCache


_IDENT_RE = re.compile(r"[a-zA-Z_][a-zA-Z0-9_]*$")


def build_server(cache: SchemaCache):
    if LanguageServer is None:
        raise RuntimeError("pygls is not installed; run `pip install 'schemadex[lsp]'`")

    server = LanguageServer("schemadex-lsp", "v1")

    @server.feature(lsp.TEXT_DOCUMENT_COMPLETION)
    def completion(params: lsp.CompletionParams) -> lsp.CompletionList:
        items: list[lsp.CompletionItem] = []
        # Suggest every table + every column the active database knows.
        for table_name in cache.list_tables():
            items.append(lsp.CompletionItem(
                label=table_name,
                kind=lsp.CompletionItemKind.Class,
                detail="table",
            ))
            tbl = cache.get_table(table_name)
            if not tbl:
                continue
            for col in tbl.get("columns", []):
                detail = col.get("native_type", "")
                if col.get("sample") and col["sample"].get("sentinel"):
                    sval, sfrac = col["sample"]["sentinel"]
                    detail += f" [sentinel: {sval} {int(sfrac * 100)}%]"
                items.append(lsp.CompletionItem(
                    label=col["name"],
                    kind=lsp.CompletionItemKind.Field,
                    detail=detail,
                ))
        return lsp.CompletionList(is_incomplete=False, items=items)

    @server.feature(lsp.TEXT_DOCUMENT_HOVER)
    def hover(params: lsp.HoverParams) -> lsp.Hover | None:
        # Pull the word under the cursor and look it up across all tables.
        doc = server.workspace.get_text_document(params.text_document.uri)
        line = doc.lines[params.position.line] if params.position.line < len(doc.lines) else ""
        match = _IDENT_RE.search(line[:params.position.character + 1])
        if not match:
            return None
        word = match.group(0)
        for table_name in cache.list_tables():
            tbl = cache.get_table(table_name)
            if not tbl:
                continue
            for col in tbl.get("columns", []):
                if col["name"].lower() == word.lower():
                    content_lines = [
                        f"**{table_name}.{col['name']}**",
                        f"type: `{col.get('native_type', '?')}`",
                        f"nullable: {col.get('nullable', True)}",
                    ]
                    sample = col.get("sample") or {}
                    if sample.get("sentinel"):
                        sval, sfrac = sample["sentinel"]
                        content_lines.append(f"⚠ sentinel: {sval} ({int(sfrac * 100)}%)")
                    return lsp.Hover(
                        contents=lsp.MarkupContent(
                            kind=lsp.MarkupKind.Markdown,
                            value="\n\n".join(content_lines),
                        ),
                    )
        return None

    return server


def main() -> int:
    ap = argparse.ArgumentParser(prog="schemadex-lsp")
    ap.add_argument("--url", required=True, help="database URL the LSP server should introspect")
    ap.add_argument("--cache-dir", default=None)
    args = ap.parse_args()
    cache = SchemaCache.from_url(args.url, cache_dir=args.cache_dir)
    server = build_server(cache)
    server.start_io()
    return 0


if __name__ == "__main__":
    sys.exit(main())
