# Security policy

`schemadex` connects to live databases and caches schema metadata on disk. Both surfaces matter for security review.

## Reporting a vulnerability

Please use [GitHub Security Advisories](https://github.com/AmiyaMandal1/schemadex/security/advisories) for private reports. Do not open a public issue for anything that could be exploited.

We aim to acknowledge reports within 7 days.

## Threat model

| Asset | Concern | Mitigation |
|-------|---------|------------|
| Database credentials | Don't leak into cache paths or logs | URLs are scrubbed of credentials before hashing (`fingerprint::scrub_url`); the cache key is a SHA-256 of the scrubbed URL. |
| Sample values | May contain PII | Sampling is opt-in (`sample_values=True`); the cache lives under `dirs::cache_dir()` with default user permissions. |
| Query construction | SQL injection through table/column identifiers | Identifiers come from `information_schema` / `pg_catalog`, not user input. Sampling SQL quotes identifiers with `"…"` to avoid case-folding surprises. |
| Cache file | Untrusted writes | Cache files are owned and written by the user running the process; we never read a cache file we didn't write. |

## Out of scope

- Crash-only DoS via maliciously crafted DDL on the live database.
- Side channels via OS-level cache file timestamps.
