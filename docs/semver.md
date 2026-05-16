# Semantic versioning policy

`schemadex` follows [SemVer 2.0.0](https://semver.org/). This document
spells out exactly what counts as a breaking change for the parts of the
codebase that have meaningful boundaries.

## What is public

### Python (`pip install schemadex`)
- Everything importable from `schemadex` (the top-level `__init__.py`
  re-exports).
- The `schemadex-mcp` console script's CLI arguments.
- The MCP tool names + parameter shapes (`list_tables`, `describe_for_agent`,
  `resolve_column`, `run_sql`, `validate_sql`, `hint_for_error`).
- The on-disk cache file format (we will read older formats forever or
  ship a migrator).

### Rust (`schemadex-core` crate, git dep)
- Everything re-exported from `lib.rs`: `SchemaCache`, `Database`, `Table`,
  `Column`, `ForeignKey`, `PrimaryKey`, `DataType`, `ColumnSample`,
  `SampleStats`, `SchemadexError`, `Result`, `ResolveResult`,
  `SchemaIntrospector`, `Backend`, `QueryRunner`, `QueryResult`,
  `describe_for_agent`, `DescribeOptions`, `resolve_column`,
  `render_table_for_agent`, `validate_sql`, `hint_for_error`,
  `assert_readonly`, `SynonymMap`, `resolve_column_with_synonyms`.
- `SchemadexError` variants (new variants are non-breaking; renames are).

### Not public
- Anything inside `crates/schemadex-py/` (PyO3 bridge — internal).
- `schemadex._native` (use `schemadex` instead).
- Backend-specific introspector structs unless you're implementing a
  custom backend (`SchemaIntrospector` is the boundary).

## What "breaking" means

| Change | Bump |
|--------|------|
| Add a new Python kwarg with a default | minor |
| Add a new public function | minor |
| Add a new MCP tool | minor |
| Add a new backend feature flag | minor |
| Rename a public function | major |
| Remove a public function | major (after a minor with `DeprecationWarning`) |
| Change a function's return shape | major |
| Add a non-defaulted kwarg | major |
| Change the cache file format incompatibly | major (and we ship a migrator) |
| Rename or remove an MCP tool | major |
| Drop a backend | major |

## Pre-1.0 caveat

Until v1.0.0 lands, any minor (`0.x.0`) may include breaking changes;
patch (`0.x.y`) won't. This is the standard 0.x convention.

After v1.0.0 the table above is the contract.
