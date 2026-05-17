//! On-disk schema cache. Layout:
//!
//! ```text
//! ~/.cache/schemadex/<url_hash>/
//!     database.json.zst       # serialized Database (current snapshot)
//!     embeddings.json.zst     # optional embedding index (Item 1)
//!     history/<unix_ts>.json.zst  # rotating snapshots (Item 2, opt-in)
//! ```
//!
//! The cache uses two layers of invalidation:
//! 1. TTL — if the file is older than `ttl`, refresh.
//! 2. Fingerprint — at refresh time, compare DDL hash; only re-introspect
//!    tables whose hash changed.

use crate::error::Result;
use crate::fingerprint::{ddl_hash, hash_database_url};
use crate::introspector::SchemaIntrospector;
use crate::memo::ResultCache;
use crate::model::{Database, Table};
use futures::future::join_all;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

/// Sentinel value written into `Table::ddl_hash` by [`SchemaCache::invalidate_table`]
/// so the next `refresh_table` call always re-introspects.
pub const INVALIDATED_DDL_HASH: &str = "__invalidated__";

#[derive(Debug, Clone)]
pub struct CacheOptions {
    pub ttl: Duration,
    pub cache_dir: Option<PathBuf>,
    pub parallel: bool,
    /// Optional history directory for time-travel snapshots. Defaults to
    /// `<cache_dir>/history/` when `history` is `Some(_)`.
    pub history_dir: Option<PathBuf>,
    /// Maximum number of historical snapshots to keep. Defaults to 10.
    pub max_history: usize,
    /// Opt-in: when `Some(n)`, the cache writes a rotating snapshot of
    /// `database.json.zst` into the history directory after every populate /
    /// refresh, keeping at most `n` snapshots. `None` disables history
    /// entirely (default — keeps disk usage bounded).
    pub history: Option<usize>,
    /// Opt-in result memoization for `run_sql` calls.
    pub memoize_results: bool,
    /// Capacity of the LRU result cache. Defaults to 128 entries.
    pub memo_capacity: usize,
}

impl Default for CacheOptions {
    fn default() -> Self {
        Self {
            ttl: Duration::from_secs(24 * 3600),
            cache_dir: None,
            parallel: true,
            history_dir: None,
            max_history: 10,
            history: None,
            memoize_results: false,
            memo_capacity: 128,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CacheEnvelope {
    saved_at_unix: u64,
    database: Database,
}

/// Persisted index of column-name embeddings. Lives next to
/// `database.json.zst` as `embeddings.json.zst`.
///
/// Keyed by table qualified name -> column name -> vector. The `model` and
/// `dim` fields are stored so callers can detect stale indexes (different
/// model, different dimensionality) and re-embed on demand.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct EmbeddingIndex {
    pub model: String,
    pub dim: usize,
    /// table -> column -> vector
    pub by_column: std::collections::BTreeMap<String, std::collections::BTreeMap<String, Vec<f32>>>,
}

pub struct SchemaCache {
    database: Database,
    cache_path: PathBuf,
    history_dir: Option<PathBuf>,
    max_history: usize,
    history_enabled: bool,
    memo: Option<Arc<ResultCache>>,
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

    /// Shared handle to the result memoization cache, if enabled. Returns
    /// `None` when `CacheOptions::memoize_results` was false at construction
    /// time. Exposed so other crates can inspect / clear the cache.
    pub fn memo(&self) -> Option<Arc<ResultCache>> {
        self.memo.clone()
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
        let history_dir = resolve_history_dir(opts, &cache_dir);
        let history_enabled = opts.history.is_some();
        let max_history = opts.history.unwrap_or(opts.max_history);
        let memo = if opts.memoize_results {
            Some(Arc::new(ResultCache::new(opts.memo_capacity)))
        } else {
            None
        };

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
                    history_dir,
                    max_history,
                    history_enabled,
                    memo,
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
        let cache = Self {
            database,
            cache_path,
            history_dir,
            max_history,
            history_enabled,
            memo,
        };
        if cache.history_enabled {
            // Best-effort: snapshot failures are logged but don't abort the
            // populate path. The primary cache file is already on disk.
            if let Err(e) = cache.snapshot().await {
                tracing::warn!(error = %e, "schema_cache.snapshot.failed");
            }
        }
        Ok(cache)
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
        // Invalidate the memo cache — fingerprint may have moved and any
        // cached result is now suspect.
        if let Some(memo) = &self.memo {
            memo.clear();
        }
        tracing::info!(
            changed = changed.len(),
            unchanged = unchanged.len(),
            "schema_cache.refresh.done"
        );
        if self.history_enabled {
            if let Err(e) = self.snapshot().await {
                tracing::warn!(error = %e, "schema_cache.snapshot.failed");
            }
        }
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
        if let Some(memo) = &self.memo {
            memo.clear();
        }
        Ok(RefreshReport { changed, unchanged })
    }

    /// Mark a single table as stale so the next [`SchemaCache::refresh_table`]
    /// call definitely re-introspects it. Useful for callers wiring Postgres
    /// logical replication or similar DDL-event streams: when the stream
    /// emits a DDL event for `users`, call `invalidate_table("users")` and
    /// the next read will be fresh.
    ///
    /// Internally sets the table's `ddl_hash` to the
    /// [`INVALIDATED_DDL_HASH`] sentinel and rewrites the persisted cache
    /// file so the invalidation survives process restarts.
    #[tracing::instrument(
        level = "info",
        name = "schema_cache.invalidate_table",
        skip(self),
        fields(
            table = qualified_or_bare_name,
        ),
    )]
    pub async fn invalidate_table(&mut self, qualified_or_bare_name: &str) -> Result<()> {
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

        self.database.tables[idx].ddl_hash = Some(INVALIDATED_DDL_HASH.to_string());

        // Recompute fingerprint so callers comparing against a prior
        // fingerprint see the change immediately.
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
        if let Some(memo) = &self.memo {
            memo.clear();
        }
        Ok(())
    }

    /// Load only — no introspection, no refresh. Used by Python callers that
    /// want to read a previously persisted cache without a live connection.
    pub async fn load(url: &str, opts: &CacheOptions) -> Result<Option<Self>> {
        let url_hash = hash_database_url(url);
        let cache_dir = resolve_cache_dir(opts, &url_hash)?;
        let cache_path = cache_dir.join("database.json.zst");
        let history_dir = resolve_history_dir(opts, &cache_dir);
        let history_enabled = opts.history.is_some();
        let max_history = opts.history.unwrap_or(opts.max_history);
        let memo = if opts.memoize_results {
            Some(Arc::new(ResultCache::new(opts.memo_capacity)))
        } else {
            None
        };
        match read_envelope(&cache_path).await? {
            Some(env) => Ok(Some(Self {
                database: env.database,
                cache_path,
                history_dir,
                max_history,
                history_enabled,
                memo,
            })),
            None => Ok(None),
        }
    }

    // -----------------------------------------------------------------------
    // Item 1: Embedding index persistence
    // -----------------------------------------------------------------------

    /// Path to the embedding index file, sibling of `database.json.zst`.
    fn embeddings_path(&self) -> PathBuf {
        let parent = self
            .cache_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        parent.join("embeddings.json.zst")
    }

    /// Read the on-disk embedding index, if any. Returns `Ok(None)` when no
    /// file is present. Errors propagate decode failures.
    #[tracing::instrument(level = "debug", name = "schema_cache.load_embeddings", skip(self))]
    pub async fn load_embeddings(&self) -> Result<Option<EmbeddingIndex>> {
        let path = self.embeddings_path();
        match tokio::fs::read(&path).await {
            Ok(bytes) => {
                let decoded = zstd::decode_all(&bytes[..]).map_err(|e| {
                    crate::error::SchemadexError::Other(format!(
                        "zstd decode failed: {e}"
                    ))
                })?;
                let idx: EmbeddingIndex = serde_json::from_slice(&decoded)?;
                Ok(Some(idx))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Persist the embedding index next to `database.json.zst`. The file is
    /// zstd-compressed JSON, same format as the schema cache itself.
    #[tracing::instrument(
        level = "debug",
        name = "schema_cache.store_embeddings",
        skip(self, idx),
        fields(model = %idx.model, dim = idx.dim, tables = idx.by_column.len()),
    )]
    pub async fn store_embeddings(&self, idx: &EmbeddingIndex) -> Result<()> {
        let path = self.embeddings_path();
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let json = serde_json::to_vec(idx)?;
        let compressed = zstd::encode_all(&json[..], 3).map_err(|e| {
            crate::error::SchemadexError::Other(format!("zstd encode failed: {e}"))
        })?;
        tokio::fs::write(&path, compressed).await?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Item 2: Time-travel snapshots
    // -----------------------------------------------------------------------

    /// Write a snapshot of the current cache into the history dir.
    /// File name is `<unix_ts>.json.zst`. Returns the path. Rotates the
    /// history dir to keep only `max_history` files.
    ///
    /// This is a no-op (returns the would-be path) when no history dir is
    /// resolvable. Callers that want snapshots without opting into
    /// auto-snapshot may invoke this method directly at any time.
    #[tracing::instrument(level = "info", name = "schema_cache.snapshot", skip(self))]
    pub async fn snapshot(&self) -> Result<PathBuf> {
        let dir = self.history_dir.clone().ok_or_else(|| {
            crate::error::SchemadexError::Other(
                "no history dir resolved for this cache".to_string(),
            )
        })?;
        tokio::fs::create_dir_all(&dir).await?;

        let unix = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        // Pick a unique filename. Two snapshots in the same second would
        // overwrite each other; append an `-N` suffix in that case so tests
        // with rapid loops still get distinct entries.
        let mut candidate = dir.join(format!("{unix}.json.zst"));
        let mut nonce: u32 = 0;
        while candidate.exists() {
            nonce += 1;
            candidate = dir.join(format!("{unix}-{nonce}.json.zst"));
        }

        let env = CacheEnvelope {
            saved_at_unix: unix,
            database: self.database.clone(),
        };
        write_envelope_at(&candidate, &env).await?;

        // Rotate: keep only the newest `max_history` files. Sort by parsed
        // timestamp prefix; fall back to mtime ordering if parsing fails.
        rotate_history(&dir, self.max_history).await?;
        Ok(candidate)
    }

    /// Read all snapshots in the history dir, sorted by timestamp ascending.
    /// Empty Vec if no history is configured or no files have been written.
    pub async fn history(&self) -> Result<Vec<(u64, Database)>> {
        let Some(dir) = self.history_dir.clone() else {
            return Ok(Vec::new());
        };
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let mut entries: Vec<(u64, PathBuf)> = Vec::new();
        let mut rd = tokio::fs::read_dir(&dir).await?;
        while let Some(e) = rd.next_entry().await? {
            let path = e.path();
            if !path.is_file() {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            // Strip the secondary `.json` (file_stem only peels the last ext).
            let stem = stem.trim_end_matches(".json");
            // Allow `<ts>` or `<ts>-<n>` filenames.
            let ts_part = stem.split('-').next().unwrap_or(stem);
            let Ok(ts) = ts_part.parse::<u64>() else {
                continue;
            };
            entries.push((ts, path));
        }
        entries.sort_by_key(|(ts, _)| *ts);

        let mut out = Vec::with_capacity(entries.len());
        for (ts, path) in entries {
            if let Some(env) = read_envelope(&path).await? {
                out.push((ts, env.database));
            }
        }
        Ok(out)
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

fn resolve_history_dir(opts: &CacheOptions, cache_dir: &Path) -> Option<PathBuf> {
    Some(
        opts.history_dir
            .clone()
            .unwrap_or_else(|| cache_dir.join("history")),
    )
}

async fn rotate_history(dir: &Path, max_history: usize) -> Result<()> {
    if max_history == 0 {
        // Special case: clear the directory entirely.
        let mut rd = tokio::fs::read_dir(dir).await?;
        while let Some(e) = rd.next_entry().await? {
            let _ = tokio::fs::remove_file(e.path()).await;
        }
        return Ok(());
    }
    let mut entries: Vec<(u64, PathBuf)> = Vec::new();
    let mut rd = tokio::fs::read_dir(dir).await?;
    while let Some(e) = rd.next_entry().await? {
        let path = e.path();
        if !path.is_file() {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let stem = stem.trim_end_matches(".json");
        let ts_part = stem.split('-').next().unwrap_or(stem);
        let Ok(ts) = ts_part.parse::<u64>() else {
            continue;
        };
        entries.push((ts, path));
    }
    if entries.len() <= max_history {
        return Ok(());
    }
    // Sort ascending (oldest first), drop the oldest extras.
    entries.sort_by_key(|(ts, _)| *ts);
    let drop_count = entries.len() - max_history;
    for (_, path) in entries.into_iter().take(drop_count) {
        let _ = tokio::fs::remove_file(&path).await;
    }
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::introspector::{Backend, QueryResult, QueryRunner};
    use crate::model::{Column, DataType, Database, Table};
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tempfile::TempDir;

    fn sample_table(name: &str, hash: &str) -> Table {
        Table {
            schema: Some("public".to_string()),
            name: name.to_string(),
            comment: None,
            columns: vec![Column {
                name: "id".to_string(),
                data_type: DataType::Integer,
                native_type: "INTEGER".to_string(),
                nullable: false,
                default: None,
                comment: None,
                ordinal: 0,
                sample: None,
                check_constraint: None,
                is_unique: true,
                generation_expression: None,
            }],
            primary_key: None,
            foreign_keys: Vec::new(),
            row_count_estimate: None,
            ddl_hash: Some(hash.to_string()),
        }
    }

    fn sample_database() -> Database {
        Database {
            backend: "sqlite".to_string(),
            url_hash: "deadbeef".to_string(),
            tables: vec![sample_table("users", "h-users"), sample_table("orders", "h-orders")],
            fingerprint: Some("fp".to_string()),
        }
    }

    async fn build_cache(tmp: &TempDir) -> SchemaCache {
        let cache_path = tmp.path().join("database.json.zst");
        let db = sample_database();
        write_envelope(&cache_path, &db).await.unwrap();
        SchemaCache {
            database: db,
            cache_path,
            history_dir: Some(tmp.path().join("history")),
            max_history: 3,
            history_enabled: true,
            memo: None,
        }
    }

    #[tokio::test]
    async fn embedding_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let cache = build_cache(&tmp).await;

        let mut idx = EmbeddingIndex {
            model: "nomic-embed-text-v2-moe".to_string(),
            dim: 3,
            by_column: Default::default(),
        };
        let mut cols = std::collections::BTreeMap::new();
        cols.insert("id".to_string(), vec![0.1f32, 0.2, 0.3]);
        cols.insert("email".to_string(), vec![0.4f32, 0.5, 0.6]);
        idx.by_column.insert("public.users".to_string(), cols);

        cache.store_embeddings(&idx).await.unwrap();
        let loaded = cache.load_embeddings().await.unwrap().expect("present");
        assert_eq!(loaded, idx);
    }

    #[tokio::test]
    async fn snapshot_rotates_to_max_history() {
        let tmp = TempDir::new().unwrap();
        let mut cache = build_cache(&tmp).await;
        cache.max_history = 3;

        // Hand-roll 12 snapshots with distinct timestamps so we don't depend
        // on the wall clock advancing between calls.
        let dir = cache.history_dir.clone().unwrap();
        tokio::fs::create_dir_all(&dir).await.unwrap();
        for i in 1u64..=12 {
            let env = CacheEnvelope {
                saved_at_unix: i,
                database: cache.database.clone(),
            };
            let path = dir.join(format!("{i}.json.zst"));
            write_envelope_at(&path, &env).await.unwrap();
            rotate_history(&dir, cache.max_history).await.unwrap();
        }

        // Count remaining `.zst` files in the history dir.
        let mut count = 0;
        let mut rd = tokio::fs::read_dir(&dir).await.unwrap();
        while let Some(e) = rd.next_entry().await.unwrap() {
            if e.path().extension().and_then(|s| s.to_str()) == Some("zst") {
                count += 1;
            }
        }
        assert_eq!(count, 3, "expected exactly 3 retained snapshots");

        // The kept snapshots must be the newest three (timestamps 10..=12).
        let hist = cache.history().await.unwrap();
        let kept_ts: Vec<u64> = hist.iter().map(|(ts, _)| *ts).collect();
        assert_eq!(kept_ts, vec![10, 11, 12]);
    }

    struct CountingRunner {
        calls: AtomicUsize,
    }

    impl CountingRunner {
        fn new() -> Self {
            Self {
                calls: AtomicUsize::new(0),
            }
        }
        fn count(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl QueryRunner for CountingRunner {
        async fn run_sql(&self, _sql: &str, _row_limit: usize) -> Result<QueryResult> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(QueryResult {
                columns: vec!["n".to_string()],
                rows: vec![vec!["1".to_string()]],
                truncated: false,
            })
        }
    }

    impl CountingRunner {
        fn _backend(&self) -> Backend {
            Backend::Sqlite
        }
    }

    #[tokio::test]
    async fn memo_hit_skips_runner() {
        let tmp = TempDir::new().unwrap();
        let cache_path = tmp.path().join("database.json.zst");
        let db = sample_database();
        write_envelope(&cache_path, &db).await.unwrap();
        let memo = Arc::new(ResultCache::new(8));
        let cache = SchemaCache {
            database: db,
            cache_path,
            history_dir: None,
            max_history: 10,
            history_enabled: false,
            memo: Some(memo.clone()),
        };
        let runner = CountingRunner::new();
        let sql = "SELECT 1";
        let _ = cache.run_sql(&runner, sql, 1024).await.unwrap();
        let _ = cache.run_sql(&runner, sql, 1024).await.unwrap();
        assert_eq!(
            runner.count(),
            1,
            "second call should hit the memo cache, not the runner"
        );
        assert_eq!(memo.size(), 1);
    }

    #[tokio::test]
    async fn preview_cost_default_runner_reports_not_supported() {
        // The CountingRunner inherits the default `preview_cost` impl,
        // which returns `bytes_processed=None`, `rows_estimate=None`, and
        // a "not supported" warning. SQLite / MySQL / DuckDB / MSSQL all
        // ride this same default in the production code.
        let tmp = TempDir::new().unwrap();
        let cache = build_cache(&tmp).await;
        let runner = CountingRunner::new();
        let est = cache.preview_cost(&runner, "SELECT 1").await.unwrap();
        assert!(est.bytes_processed.is_none());
        assert!(est.rows_estimate.is_none());
        let w = est.warning.as_deref().unwrap_or("");
        assert!(w.contains("not supported"), "warning was {w:?}");
    }

    #[tokio::test]
    async fn invalidate_marks_ddl_hash() {
        let tmp = TempDir::new().unwrap();
        let mut cache = build_cache(&tmp).await;
        cache.invalidate_table("users").await.unwrap();
        let users = cache.database.table("users").unwrap();
        assert_eq!(users.ddl_hash.as_deref(), Some(INVALIDATED_DDL_HASH));
    }
}
