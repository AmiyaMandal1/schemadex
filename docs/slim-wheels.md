# Slim wheels

## The situation

The default `pip install schemadex` wheel bundles every backend
(`postgres`, `sqlite`, `mysql`, `duckdb_backend`, `bigquery`, `snowflake`,
`mssql`). DuckDB alone weighs ~20 MB statically linked, so the all-backends
wheel is heavy.

Ideally we would ship a separate slim wheel and let users opt into backends
via extras (`pip install schemadex[postgres]`). That isn't possible today:
maturin produces one wheel per build, and PEP 508 extras can only pull in
extra *Python* dependencies — they can't switch which compiled artifact you
get. You can't differentiate wheels by extra without publishing them as
separate distributions, and we don't want to fragment the PyPI namespace.

The default wheel will stay heavy on PyPI. Power users who care about size
can build a slim wheel from source.

## Building a slim wheel from source

The `schemadex-py` crate exposes a `slim` feature with no backends and a
`full` feature equivalent to `default`. Pair `--no-default-features` with
the backends you actually need:

```bash
# postgres only
maturin build --release --no-default-features \
  --features schemadex-py/postgres

# postgres + sqlite
maturin build --release --no-default-features \
  --features schemadex-py/postgres,schemadex-py/sqlite

# all backends (same as the default PyPI wheel)
maturin build --release --no-default-features \
  --features schemadex-py/full
```

Pass `--out dist` to drop the wheel into `dist/` and install it with
`pip install dist/schemadex-*.whl`.

## Why the `full` extra is empty

```toml
[project.optional-dependencies]
full = []
```

This row in `pyproject.toml` is cosmetic. It exists so users who type
`pip install schemadex[full]` get something rather than a hard error. The
default wheel already includes every backend, so the extra resolves to no
additional Python deps.
