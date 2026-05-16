//! On-disk schema cache. Layout:
//!
//! ```text
//! ~/.cache/schemadex/<url_hash>/
//!     database.json           # serialized Database
//!     <table_hash>.json       # per-table ddl signatures (future, kept for granular invalidation)
//! ```
//!
//! The cache uses two layers of invalidation:
//! 1. TTL — if the file is older than `ttl`, refresh.
//! 2. Fingerprint — at refresh time, compare DDL hash; only re-introspect
//!    tables whose hash changed.

use crate::error::Result;
use crate::fingerprint::{ddl_hash, hash_database_url};
use crate::introspector::SchemaIntrospector;
use crate::model::{Database, Table};
use futures::future::join_all;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

#[derive(Debug, Clone)]
pub struct CacheOptions {
    pub ttl: Duration,
    pub cache_dir: Option<PathBuf>,
    pub parallel: bool,
}

impl Default for CacheOptions {
    fn default() -> Self {
        Self {
            ttl: Duration::from_secs(24 * 3600),
            cache_dir: None,
            parallel: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CacheEnvelope {
    saved_at_unix: u64,
    database: Database,
}

pub struct SchemaCache {
    database: Database,
    cache_path: PathBuf,
}

impl SchemaCache {
    pub fn database(&self) -> &Database {
        &self.database
    }

    pub fn into_database(self) -> Database {
        self.database
    }

    pub fn cache_path(&self) -> &Path {
        &self.cache_path
    }

    /// Build a fresh cache by introspecting via the given backend, then
    /// persist it to disk. If a fresh on-disk cache exists, reuse it.
    #[tracing::instrument(
        level = "info",
        name = "schema_cache.from_introspector",
        skip(introspector, opts),
        fields(
            backend = introspector.backend().as_str(),
            url_hash = tracing::field::Empty,
            ttl_secs = opts.ttl.as_secs(),
            parallel = opts.parallel,
        ),
    )]
    pub async fn from_introspector<I: SchemaIntrospector + ?Sized>(
        introspector: &I,
        url: &str,
        opts: &CacheOptions,
    ) -> Result<Self> {
        let url_hash = hash_database_url(url);
        tracing::Span::current().record("url_hash", url_hash.as_str());
        let cache_dir = resolve_cache_dir(opts, &url_hash)?;
        tokio::fs::create_dir_all(&cache_dir).await?;
        let cache_path = cache_dir.join("database.json.zst");

        if let Some(existing) = read_envelope(&cache_path).await? {
            if !is_stale(&existing, opts.ttl) {
                tracing::info!(
                    cache_path = %cache_path.display(),
                    table_count = existing.database.tables.len(),
                    "schema_cache.hit"
                );
                return Ok(Self {
                    database: existing.database,
                    cache_path,
                });
            }
            tracing::info!(cache_path = %cache_path.display(), "schema_cache.stale");
        } else {
            tracing::info!(cache_path = %cache_path.display(), "schema_cache.miss");
        }

        let database = introspect_all(introspector, &url_hash, opts.parallel).await?;
        write_envelope(&cache_path, &database).await?;
        tracing::info!(
            cache_path = %cache_path.display(),
            table_count = database.tables.len(),
            "schema_cache.populated"
        );
        Ok(Self {
            database,
            cache_path,
        })
    }

    /// Force a refresh: re-introspect every table, but only rewrite the cache
    /// entries whose DDL hash actually changed.
    #[tracing::instrument(
        level = "info",
        name = "schema_cache.refresh",
        skip(self, introspector),
        fields(
            backend = introspector.backend().as_str(),
            url_hash = %self.database.url_hash,
            parallel,
            tables_before = self.database.tables.len(),
        ),
    )]
    pub async fn refresh<I: SchemaIntrospector + ?Sized>(
        &mut self,
        introspector: &I,
        parallel: bool,
    ) -> Result<RefreshReport> {
        let url_hash = self.database.url_hash.clone();
        let fresh = introspect_all(introspector, &url_hash, parallel).await?;

        let mut changed = Vec::new();
        let mut unchanged = Vec::new();
        for new_tbl in &fresh.tables {
            let qn = new_tbl.qualified_name();
            let prev = self.database.table(&qn);
            match (
                prev.and_then(|p| p.ddl_hash.clone()),
                new_tbl.ddl_hash.clone(),
            ) {
                (Some(a), Some(b)) if a == b => unchanged.push(qn),
                _ => changed.push(qn),
            }
        }

        self.database = fresh;
        write_envelope(&self.cache_path, &self.database).await?;
        tracing::info!(
            changed = changed.len(),
            unchanged = unchanged.len(),
            "schema_cache.refresh.done"
        );
        Ok(RefreshReport { changed, unchanged })
    }

    /// Refresh a single table by qualified name (e.g. `public.users`) or bare
    /// name (e.g. `users`). Matching is case-insensitive and mirrors
    /// [`Database::table`].
    ///
    /// Returns a [`RefreshReport`] where the table appears in either
    /// `changed` or `unchanged` based on its DDL hash. Errors with
    /// [`SchemadexError::TableNotFound`] if the name doesn't match any
    /// currently cached table.
    #[tracing::instrument(
        level = "info",
        name = "schema_cache.refresh_table",
        skip(self, introspector),
        fields(
            backend = introspector.backend().as_str(),
            url_hash = %self.database.url_hash,
            table = qualified_or_bare_name,
        ),
    )]
    pub async fn refresh_table<I: SchemaIntrospector + ?Sized>(
        &mut self,
        introspector: &I,
        qualified_or_bare_name: &str,
    ) -> Result<RefreshReport> {
        // Locate the existing table in the cache. We need its (schema, name)
        // pair to pass to the introspector, plus its index for swap-in-place.
        let idx = self
            .database
            .tables
            .iter()
            .position(|t| {
                t.name.eq_ignore_ascii_case(qualified_or_bare_name)
                    || t.qualified_name()
                        .eq_ignore_ascii_case(qualified_or_bare_name)
            })
            .ok_or_else(|| {
                crate::error::SchemadexError::TableNotFound(qualified_or_bare_name.to_string())
            })?;

        let (schema, name) = {
            let t = &self.database.tables[idx];
            (t.schema.clone(), t.name.clone())
        };

        let mut fresh = introspector
            .introspect_table(schema.as_deref(), &name)
            .await?;
        let sig = introspector
            .ddl_signature(schema.as_deref(), &name)
            .await?;
        fresh.ddl_hash = Some(ddl_hash(&sig));

        let prev_hash = self.database.tables[idx].ddl_hash.clone();
        let qn = fresh.qualified_name();
        let mut changed = Vec::new();
        let mut unchanged = Vec::new();
        match (prev_hash, fresh.ddl_hash.clone()) {
            (Some(a), Some(b)) if a == b => unchanged.push(qn),
            _ => changed.push(qn),
        }

        self.database.tables[idx] = fresh;
        self.database.tables.sort_by_key(|a| a.qualified_name());

        // Recompute the database fingerprint after the swap.
        let fingerprint_input = self
            .database
            .tables
            .iter()
            .filter_map(|t| t.ddl_hash.as_deref())
            .collect::<Vec<_>>()
            .join(":");
        self.database.fingerprint = if fingerprint_input.is_empty() {
            None
        } else {
            Some(ddl_hash(&fingerprint_input))
        };

        write_envelope(&self.cache_path, &self.database).await?;
        Ok(RefreshReport { changed, unchanged })
    }

    /// Load only — no introspection, no refresh. Used by Python callers that
    /// want to read a previously persisted cache without a live connection.
    pub async fn load(url: &str, opts: &CacheOptions) -> Result<Option<Self>> {
        let url_hash = hash_database_url(url);
        let cache_dir = resolve_cache_dir(opts, &url_hash)?;
        let cache_path = cache_dir.join("database.json.zst");
        match read_envelope(&cache_path).await? {
            Some(env) => Ok(Some(Self {
                database: env.database,
                cache_path,
            })),
            None => Ok(None),
        }
    }
}

#[derive(Debug, Clone)]
pub struct RefreshReport {
    pub changed: Vec<String>,
    pub unchanged: Vec<String>,
}

fn resolve_cache_dir(opts: &CacheOptions, url_hash: &str) -> Result<PathBuf> {
    let base = match &opts.cache_dir {
        Some(p) => p.clone(),
        None => dirs::cache_dir()
            .ok_or_else(|| {
                crate::error::SchemadexError::Other("could not resolve cache dir".to_string())
            })?
            .join("schemadex"),
    };
    Ok(base.join(url_hash))
}

async fn introspect_all<I: SchemaIntrospector + ?Sized>(
    introspector: &I,
    url_hash: &str,
    parallel: bool,
) -> Result<Database> {
    let names = introspector.tables().await?;
    let mut tables: Vec<Table> = if parallel {
        let futures: Vec<_> = names
            .iter()
            .map(|(schema, name)| async move {
                let mut t = introspector
                    .introspect_table(schema.as_deref(), name)
                    .await?;
                let sig = introspector.ddl_signature(schema.as_deref(), name).await?;
                t.ddl_hash = Some(ddl_hash(&sig));
                Ok::<_, crate::error::SchemadexError>(t)
            })
            .collect();
        let results = join_all(futures).await;
        let mut out = Vec::with_capacity(results.len());
        for r in results {
            out.push(r?);
        }
        out
    } else {
        let mut out = Vec::with_capacity(names.len());
        for (schema, name) in &names {
            let mut t = introspector
                .introspect_table(schema.as_deref(), name)
                .await?;
            let sig = introspector.ddl_signature(schema.as_deref(), name).await?;
            t.ddl_hash = Some(ddl_hash(&sig));
            out.push(t);
        }
        out
    };
    tables.sort_by_key(|a| a.qualified_name());

    let fingerprint_input = tables
        .iter()
        .filter_map(|t| t.ddl_hash.as_deref())
        .collect::<Vec<_>>()
        .join(":");
    let fingerprint = if fingerprint_input.is_empty() {
        None
    } else {
        Some(ddl_hash(&fingerprint_input))
    };

    Ok(Database {
        backend: introspector.backend().as_str().to_string(),
        url_hash: url_hash.to_string(),
        tables,
        fingerprint,
    })
}

async fn read_envelope(path: &Path) -> Result<Option<CacheEnvelope>> {
    // Primary path: zstd-compressed envelope at `database.json.zst`.
    match tokio::fs::read(path).await {
        Ok(bytes) => {
            let decoded = zstd::decode_all(&bytes[..]).map_err(|e| {
                crate::error::SchemadexError::Other(format!("zstd decode failed: {e}"))
            })?;
            let env: CacheEnvelope = serde_json::from_slice(&decoded)?;
            Ok(Some(env))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // Legacy migration: pre-0.9 caches sat at `database.json` (pretty
            // JSON, uncompressed). If we find one, decompress-and-rewrite it
            // once so the next read hits the new path.
            let legacy = legacy_path_for(path);
            match tokio::fs::read(&legacy).await {
                Ok(bytes) => {
                    let env: CacheEnvelope = serde_json::from_slice(&bytes)?;
                    write_envelope_at(path, &env).await?;
                    // Drop the legacy file so we don't keep migrating it
                    // every read. Failure here is non-fatal — the new file is
                    // already in place.
                    let _ = tokio::fs::remove_file(&legacy).await;
                    Ok(Some(env))
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(e.into()),
            }
        }
        Err(e) => Err(e.into()),
    }
}

fn legacy_path_for(path: &Path) -> PathBuf {
    // `database.json.zst` -> `database.json`. If a caller hands us a path
    // without the `.zst` suffix we leave it alone.
    if path.extension().and_then(|s| s.to_str()) == Some("zst") {
        path.with_extension("")
    } else {
        path.to_path_buf()
    }
}

async fn write_envelope(path: &Path, db: &Database) -> Result<()> {
    let env = CacheEnvelope {
        saved_at_unix: SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        database: db.clone(),
    };
    write_envelope_at(path, &env).await
}

async fn write_envelope_at(path: &Path, env: &CacheEnvelope) -> Result<()> {
    // Compress with level 3 — a reasonable speed/ratio default for JSON.
    let json = serde_json::to_vec(env)?;
    let compressed = zstd::encode_all(&json[..], 3).map_err(|e| {
        crate::error::SchemadexError::Other(format!("zstd encode failed: {e}"))
    })?;
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(path, compressed).await?;
    Ok(())
}

fn is_stale(env: &CacheEnvelope, ttl: Duration) -> bool {
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    now.saturating_sub(env.saved_at_unix) > ttl.as_secs()
}
