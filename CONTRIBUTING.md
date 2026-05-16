# Contributing

Thanks for your interest. This project is pre-1.0 — the simplest path is to **open an issue first** before sending a large PR, so we can align on scope.

## Dev setup

```bash
# Rust
cargo test -p schemadex-core --no-default-features
cargo test -p schemadex-core --features "postgres,sqlite,duckdb_backend"
# MSRV: Rust 1.80 (Cargo.lock format v4)
cargo clippy --workspace --all-features -- -D warnings
cargo fmt --all -- --check

# Python (requires uv + a recent maturin)
uv python install 3.11
uv venv
uv pip install -e ".[dev]"
maturin develop --release
pytest
```

## PR checklist

- [ ] Tests added or updated.
- [ ] `CHANGELOG.md` has an entry under `## [Unreleased]`.
- [ ] `cargo fmt` clean, `cargo clippy` clean.
- [ ] No new `unwrap()` on a path that takes external input.

## Reporting bugs

Use the issue template. For anything that looks like a security issue, follow [SECURITY.md](./SECURITY.md) instead.
