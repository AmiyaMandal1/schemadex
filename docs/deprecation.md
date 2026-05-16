# Deprecation policy

Public APIs that we intend to remove or change incompatibly get a
deprecation period.

## Process

1. **Minor release N**: emit a `DeprecationWarning` (Python) or
   `#[deprecated]` attribute (Rust) whenever the deprecated API is called.
   The warning text names the replacement and the planned removal version.
2. **At least one full minor cycle later** (N+1 or later): the deprecated
   API is removed in a major release.
3. **Never silent**: deprecations are listed in `CHANGELOG.md` under
   `### Deprecated` for the release that introduces them and
   `### Removed` for the release that completes the removal.

## Exceptions

- Security fixes. We will break API to close a vulnerability and bump
  the major immediately, with a `SECURITY.md` advisory.
- Pre-1.0 minors (`0.x.0`). Deprecation may compress to a single minor
  cycle.
