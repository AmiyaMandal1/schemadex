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
    pub async fn from_introspector<I: SchemaIntrospector + ?Sized>(
        introspector: &I,
        url: &str,
        opts: &CacheOptions,
    ) -> Result<Self> {
        let url_hash = hash_database_url(url);
        let cache_dir = resolve_cache_dir(opts, &url_hash)?;
        tokio::fs::create_dir_all(&cache_dir).await?;
        let cache_path = cache_dir.join("database.json");

        if let Some(existing) = read_envelope(&cache_path).await? {
            if !is_stale(&existing, opts.ttl) {
                return Ok(Self {
                    database: existing.database,
                    cache_path,
                });
            }
        }

        let database = introspect_all(introspector, &url_hash, opts.parallel).await?;
        write_envelope(&cache_path, &database).await?;
        Ok(Self {
            database,
            cache_path,
        })
    }

    /// Force a refresh: re-introspect every table, but only rewrite the cache
    /// entries whose DDL hash actually changed.
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
        Ok(RefreshReport { changed, unchanged })
    }

    /// Load only — no introspection, no refresh. Used by Python callers that
    /// want to read a previously persisted cache without a live connection.
    pub async fn load(url: &str, opts: &CacheOptions) -> Result<Option<Self>> {
        let url_hash = hash_database_url(url);
        let cache_dir = resolve_cache_dir(opts, &url_hash)?;
        let cache_path = cache_dir.join("database.json");
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
    match tokio::fs::read(path).await {
        Ok(bytes) => {
            let env: CacheEnvelope = serde_json::from_slice(&bytes)?;
            Ok(Some(env))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
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
    let bytes = serde_json::to_vec_pretty(&env)?;
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(path, bytes).await?;
    Ok(())
}

fn is_stale(env: &CacheEnvelope, ttl: Duration) -> bool {
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    now.saturating_sub(env.saved_at_unix) > ttl.as_secs()
}
