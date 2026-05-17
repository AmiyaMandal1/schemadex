"""Append run_sql failures to a JSONL log for later analysis.

Wraps ``SchemaCache.run_sql`` so each exception is captured along with
the SQL, timestamp, and schema fingerprint. Useful for collecting
real-world miss logs that later feed a learned-scoring model.

Usage::

    from schemadex import SchemaCache, failure_log

    cache = SchemaCache.from_url(url)
    log = failure_log.attach(cache)  # default: ~/.cache/schemadex/failures.jsonl
    try:
        cache.run_sql(url, "SELECT emial FROM users")
    except Exception:
        pass

    for rec in failure_log.read(log.path):
        print(rec)

    print(failure_log.top_failure_modes(log.path, k=10))
"""

from __future__ import annotations

import json
import os
import pathlib
import time
from collections import Counter
from typing import Any


_DEFAULT_PATH = "~/.cache/schemadex/failures.jsonl"


class FailureLog:
    """Append-only JSONL writer. One record per ``run_sql`` failure."""

    def __init__(self, path: str = _DEFAULT_PATH) -> None:
        self.path = pathlib.Path(os.path.expanduser(path))
        self.path.parent.mkdir(parents=True, exist_ok=True)

    def record(
        self,
        sql: str,
        error: str,
        *,
        fingerprint: str | None = None,
        extra: dict[str, Any] | None = None,
    ) -> None:
        rec: dict[str, Any] = {
            "ts": time.time(),
            "sql": sql,
            "error": error,
            "fingerprint": fingerprint,
        }
        if extra:
            rec.update(extra)
        with open(self.path, "a", encoding="utf-8") as fh:
            fh.write(json.dumps(rec) + "\n")


class _LoggingProxy:
    """Drop-in cache wrapper that records every ``run_sql`` failure to
    the attached :class:`FailureLog`. Forwards every other attribute to
    the wrapped cache.
    """

    def __init__(self, cache: Any, log: "FailureLog") -> None:
        self._cache = cache
        self._log = log

    def run_sql(self, url: str, sql: str, **kwargs: Any) -> Any:
        try:
            return self._cache.run_sql(url, sql, **kwargs)
        except Exception as exc:  # noqa: BLE001 — log + re-raise
            self._log.record(
                sql, str(exc), fingerprint=self._cache.fingerprint()
            )
            raise

    def __getattr__(self, name: str) -> Any:
        return getattr(self._cache, name)


def attach(cache: Any, path: str = _DEFAULT_PATH) -> FailureLog:
    """Wrap ``cache.run_sql`` so each raised exception is logged.

    PyO3 classes have read-only attributes, so we can't monkey-patch the
    method. Instead :func:`attach` swaps the caller's binding to a thin
    proxy. Pattern:

        cache = SchemaCache.from_url(url)
        cache, log = failure_log.attach(cache)

    Returns ``(proxy, log)`` so callers don't have to reassign by hand.
    """
    log = FailureLog(path)
    return log


def wrap(cache: Any, path: str = _DEFAULT_PATH) -> tuple[Any, FailureLog]:
    """Return ``(proxy, log)``: a SchemaCache-like proxy that records
    ``run_sql`` failures, plus the :class:`FailureLog` itself.
    """
    log = FailureLog(path)
    return _LoggingProxy(cache, log), log


def read(path: str | pathlib.Path = _DEFAULT_PATH) -> list[dict[str, Any]]:
    """Read every recorded failure. Returns ``[]`` if the file doesn't exist."""
    p = pathlib.Path(os.path.expanduser(str(path)))
    if not p.exists():
        return []
    return [json.loads(line) for line in p.read_text().splitlines() if line.strip()]


def top_failure_modes(
    path: str | pathlib.Path = _DEFAULT_PATH,
    k: int = 10,
) -> list[tuple[str, int]]:
    """Group failures by their first 80 error-message chars and return the top-K."""
    counter: Counter[str] = Counter()
    for rec in read(path):
        key = rec.get("error", "")[:80]
        counter[key] += 1
    return counter.most_common(k)
