# Migrating from 0.x to 1.0

This guide covers every breaking change between the last `0.x` release
and `1.0.0`. If a section doesn't mention something you depend on, it
hasn't changed.

## Python

(populate this section as 1.0 PRs land; today the 0.x API is the v1.0 API
verbatim. We commit to filling this in before tagging v1.0.0.)

## Rust

(same as above. The trait set in `crates/schemadex-core/src/introspector.rs`
is the public boundary.)

## Cache format

The cache file moved from `database.json` to `database.json.zst` in v0.9.
The cache code reads either path transparently and migrates on first
write. No user action required.

## MCP

No tool renames or removals between 0.x and 1.0. New tools added in v0.7
(`validate_sql`, `hint_for_error`) are still present.
