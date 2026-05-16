"""Tiny local web dashboard for schemadex.

Usage:
    schemadex-serve --url postgres://... --port 8080

Open http://localhost:8080 in a browser.

Routes:
    GET /              -> dashboard HTML
    GET /api/tables    -> JSON list of tables
    GET /api/table/<name> -> JSON for one table
    GET /api/resolve?table=...&candidate=... -> resolve_column JSON
"""

from __future__ import annotations

import argparse
import http.server
import json
import sys
import urllib.parse
from typing import Any

from schemadex import SchemaCache


_INDEX_HTML = """<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <title>schemadex</title>
  <style>
    body { font-family: -apple-system, system-ui, sans-serif; max-width: 1200px; margin: 2em auto; padding: 0 1em; }
    h1 { font-size: 1.5em; margin-bottom: 0.2em; }
    .meta { color: #666; font-size: 0.9em; margin-bottom: 1.5em; }
    .tables { display: grid; grid-template-columns: 220px 1fr; gap: 1em; }
    .table-list { border-right: 1px solid #ddd; padding-right: 1em; }
    .table-list ul { list-style: none; padding: 0; margin: 0; }
    .table-list li { padding: 0.3em 0; cursor: pointer; }
    .table-list li:hover { background: #f5f5f5; }
    .table-list li.selected { background: #e0e8ff; }
    table { border-collapse: collapse; width: 100%; font-size: 0.9em; }
    th, td { text-align: left; padding: 0.4em 0.6em; border-bottom: 1px solid #eee; }
    th { background: #fafafa; }
    .sentinel { color: #c33; font-weight: 600; }
    .resolve { margin-top: 2em; padding: 1em; background: #fafafa; border-radius: 4px; }
    .resolve input { width: 220px; padding: 0.3em; margin: 0 0.4em 0 0; }
    .resolve-out { margin-top: 0.5em; font-family: ui-monospace, monospace; }
  </style>
</head>
<body>
  <h1>schemadex</h1>
  <div class="meta" id="meta">loading...</div>
  <div class="tables">
    <div class="table-list">
      <ul id="table-list"></ul>
    </div>
    <div class="table-detail" id="table-detail">
      <em>pick a table</em>
    </div>
  </div>
  <div class="resolve">
    <strong>Resolve column</strong><br>
    <input id="r-table" placeholder="table name">
    <input id="r-candidate" placeholder="candidate column">
    <button onclick="resolve()">Resolve</button>
    <div class="resolve-out" id="r-out"></div>
  </div>
<script>
async function load() {
  const r = await fetch('/api/tables');
  const data = await r.json();
  document.getElementById('meta').textContent = data.length + ' tables';
  const ul = document.getElementById('table-list');
  for (const name of data) {
    const li = document.createElement('li');
    li.textContent = name;
    li.onclick = () => pickTable(name, li);
    ul.appendChild(li);
  }
}
async function pickTable(name, li) {
  document.querySelectorAll('.table-list li').forEach(el => el.classList.remove('selected'));
  li.classList.add('selected');
  const r = await fetch('/api/table/' + encodeURIComponent(name));
  const t = await r.json();
  if (!t) {
    document.getElementById('table-detail').innerHTML = '<em>not found</em>';
    return;
  }
  let html = '<h2>' + name + '</h2>';
  if (t.comment) html += '<p>' + t.comment + '</p>';
  html += '<table><thead><tr><th>Column</th><th>Type</th><th>Null</th><th>Sample</th></tr></thead><tbody>';
  for (const c of t.columns) {
    html += '<tr><td>' + c.name + '</td><td>' + c.native_type + '</td><td>' + (c.nullable ? 'YES' : 'NO') + '</td><td>';
    if (c.sample && c.sample.sentinel) {
      html += '<span class="sentinel">sentinel: ' + c.sample.sentinel[0] + ' (' + Math.round(c.sample.sentinel[1] * 100) + '%)</span>';
    } else if (c.sample && c.sample.top_values && c.sample.top_values.length) {
      html += c.sample.top_values.slice(0, 3).map(v => v[0] + ' (' + Math.round(v[1] * 100) + '%)').join(', ');
    }
    html += '</td></tr>';
  }
  html += '</tbody></table>';
  document.getElementById('table-detail').innerHTML = html;
}
async function resolve() {
  const table = document.getElementById('r-table').value;
  const candidate = document.getElementById('r-candidate').value;
  if (!table || !candidate) return;
  const r = await fetch('/api/resolve?table=' + encodeURIComponent(table) + '&candidate=' + encodeURIComponent(candidate));
  const data = await r.json();
  document.getElementById('r-out').textContent = JSON.stringify(data, null, 2);
}
load();
</script>
</body>
</html>
"""


def _make_handler(cache: SchemaCache):
    """Build a BaseHTTPRequestHandler subclass that closes over ``cache``.

    Returning a class (not an instance) keeps the stdlib ``http.server``
    convention: ``HTTPServer`` instantiates one handler per request and we
    can't pass extra constructor args without subclassing. Closing over the
    cache via a factory is the idiomatic workaround.
    """

    class Handler(http.server.BaseHTTPRequestHandler):
        def log_message(self, fmt: str, *args: Any) -> None:  # noqa: A002
            # Silence default stderr access log — keeps test output clean and
            # avoids leaking request lines if the caller pipes stderr around.
            return

        def _json(self, payload: Any, status: int = 200) -> None:
            body = json.dumps(payload).encode("utf-8")
            self.send_response(status)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def do_GET(self) -> None:  # noqa: N802 - stdlib signature
            parsed = urllib.parse.urlparse(self.path)
            path = parsed.path
            if path == "/" or path == "/index.html":
                body = _INDEX_HTML.encode("utf-8")
                self.send_response(200)
                self.send_header("Content-Type", "text/html; charset=utf-8")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)
                return
            if path == "/api/tables":
                self._json(cache.list_tables())
                return
            if path.startswith("/api/table/"):
                name = urllib.parse.unquote(path[len("/api/table/"):])
                self._json(cache.get_table(name))
                return
            if path == "/api/resolve":
                qs = urllib.parse.parse_qs(parsed.query)
                table = qs.get("table", [""])[0]
                candidate = qs.get("candidate", [""])[0]
                if not table or not candidate:
                    self._json({"error": "missing table/candidate"}, status=400)
                    return
                r = cache.resolve(table, candidate)
                self._json(
                    {
                        "matched": r.matched,
                        "confidence": r.confidence,
                        "alternatives": r.alternatives,
                    }
                )
                return
            self.send_response(404)
            self.end_headers()

    return Handler


def main() -> int:
    ap = argparse.ArgumentParser(prog="schemadex-serve")
    ap.add_argument(
        "--url",
        required=True,
        help="Database URL (postgres://..., sqlite://...)",
    )
    ap.add_argument(
        "--port",
        type=int,
        default=8080,
        help="Port to bind on 127.0.0.1 (default: 8080)",
    )
    ap.add_argument(
        "--cache-dir",
        default=None,
        help="Override the on-disk cache directory.",
    )
    args = ap.parse_args()
    cache = SchemaCache.from_url(args.url, cache_dir=args.cache_dir)
    handler = _make_handler(cache)
    server = http.server.ThreadingHTTPServer(("127.0.0.1", args.port), handler)
    print(
        f"schemadex-serve: http://127.0.0.1:{args.port} "
        f"-> {len(cache.list_tables())} tables"
    )
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    server.server_close()
    return 0


if __name__ == "__main__":
    sys.exit(main())
