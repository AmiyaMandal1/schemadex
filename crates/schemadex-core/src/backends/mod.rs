//! Backend implementations. Each backend lives behind a feature flag so
//! a thin wheel can ship one driver instead of all three.

#[cfg(feature = "postgres")]
pub mod postgres;

#[cfg(feature = "sqlite")]
pub mod sqlite;

#[cfg(feature = "mysql")]
pub mod mysql;

#[cfg(feature = "duckdb_backend")]
pub mod duckdb;

#[cfg(feature = "bigquery")]
pub mod bigquery;

#[cfg(feature = "snowflake")]
pub mod snowflake;

#[cfg(feature = "mssql")]
pub mod mssql;

use crate::error::{Result, SchemadexError};
use crate::introspector::{QueryRunner, SchemaIntrospector};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

/// Dispatch a URL to the right backend. Returns a boxed introspector.
///
/// Thin wrapper around [`connect_with_sampling`] for callers that don't want
/// sample-value collection.
pub async fn connect(url: &str) -> Result<Arc<dyn SchemaIntrospector>> {
    connect_with_sampling(url, None).await
}

/// Dispatch a URL to the right backend, optionally enabling sample-value
/// collection on backends that support it.
///
/// Postgres, SQLite, MySQL, and DuckDB all honor `sampling`. BigQuery,
/// Snowflake, and MSSQL ignore it for now.
pub async fn connect_with_sampling(
    url: &str,
    sampling: Option<crate::sampling::SamplingPolicy>,
) -> Result<Arc<dyn SchemaIntrospector>> {
    let scheme = url.split_once(':').map(|(s, _)| s).unwrap_or("");
    match scheme {
        #[cfg(feature = "postgres")]
        "postgres" | "postgresql" => {
            let introspector = postgres::PostgresIntrospector::connect(url).await?;
            let introspector = if let Some(policy) = sampling {
                introspector.with_sampling(policy)
            } else {
                introspector
            };
            Ok(Arc::new(introspector))
        }
        #[cfg(feature = "sqlite")]
        "sqlite" | "file" => {
            let mut introspector = sqlite::SqliteIntrospector::connect(url).await?;
            if let Some(policy) = sampling {
                introspector = introspector.with_sampling(policy);
            }
            Ok(Arc::new(introspector))
        }
        #[cfg(feature = "mysql")]
        "mysql" | "mariadb" => {
            let mut introspector = mysql::MysqlIntrospector::connect(url).await?;
            if let Some(policy) = sampling {
                introspector = introspector.with_sampling(policy);
            }
            Ok(Arc::new(introspector))
        }
        #[cfg(feature = "duckdb_backend")]
        "duckdb" => {
            let mut introspector = duckdb::DuckDbIntrospector::connect(url)?;
            if let Some(policy) = sampling {
                introspector = introspector.with_sampling(policy);
            }
            Ok(Arc::new(introspector))
        }
        #[cfg(feature = "bigquery")]
        "bigquery" => {
            let _ = sampling;
            Ok(Arc::new(
                bigquery::BigQueryIntrospector::connect(url).await?,
            ))
        }
        #[cfg(feature = "snowflake")]
        "snowflake" => {
            let _ = sampling;
            Ok(Arc::new(
                snowflake::SnowflakeIntrospector::connect(url).await?,
            ))
        }
        #[cfg(feature = "mssql")]
        "mssql" | "sqlserver" => {
            let _ = sampling;
            Ok(Arc::new(mssql::MssqlIntrospector::connect(url).await?))
        }
        other => Err(SchemadexError::UnsupportedScheme(other.to_string())),
    }
}

/// Dispatch a URL to the right backend and return a [`QueryRunner`] for ad-hoc
/// SELECTs. Mirrors [`connect`] but hands back the narrower trait object that
/// `SchemaCache::run_sql` needs.
///
/// DuckDB is not yet wired up here — it uses a synchronous `rusqlite`-style
/// connection model that doesn't fit cleanly behind the async trait.
pub async fn connect_runner(url: &str) -> Result<Arc<dyn QueryRunner>> {
    let scheme = url.split_once(':').map(|(s, _)| s).unwrap_or("");
    match scheme {
        #[cfg(feature = "postgres")]
        "postgres" | "postgresql" => Ok(Arc::new(
            postgres::PostgresIntrospector::connect(url).await?,
        )),
        #[cfg(feature = "sqlite")]
        "sqlite" | "file" => Ok(Arc::new(sqlite::SqliteIntrospector::connect(url).await?)),
        #[cfg(feature = "mysql")]
        "mysql" | "mariadb" => Ok(Arc::new(mysql::MysqlIntrospector::connect(url).await?)),
        #[cfg(feature = "bigquery")]
        "bigquery" => Ok(Arc::new(
            bigquery::BigQueryIntrospector::connect(url).await?,
        )),
        #[cfg(feature = "snowflake")]
        "snowflake" => Ok(Arc::new(
            snowflake::SnowflakeIntrospector::connect(url).await?,
        )),
        #[cfg(feature = "mssql")]
        "mssql" | "sqlserver" => {
            Ok(Arc::new(mssql::MssqlIntrospector::connect(url).await?))
        }
        // TODO: wire up DuckDB once we decide how to bridge its sync API into
        // the async QueryRunner trait.
        other => Err(SchemadexError::UnsupportedScheme(other.to_string())),
    }
}

// ---------------------------------------------------------------------------
// Process-wide runner pool
// ---------------------------------------------------------------------------
//
// `connect_runner` builds a fresh introspector + connection on every call,
// which is fine for one-shot CLI usage but wasteful for a long-lived Python
// process that issues many `run_sql` calls. The pool below caches each runner
// by its URL string so the second call onward reuses the existing connection
// pool inside the backend's introspector.
//
// Cache key: raw URL (process-local cache — credentials are not exfiltrated).
// We use a `std::sync::Mutex` because the only critical section is a hashmap
// lookup; the actual (potentially-await-ing) backend build happens outside
// the lock to avoid deadlocking the runtime.

type RunnerPool = Mutex<HashMap<String, Arc<dyn QueryRunner>>>;

fn pool() -> &'static RunnerPool {
    static POOL: OnceLock<RunnerPool> = OnceLock::new();
    POOL.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Return a shared [`QueryRunner`] for `url`, building one on first call and
/// reusing it on subsequent calls. The cache lives for the lifetime of the
/// process and is keyed by the URL string.
///
/// Two callers racing on the same URL may both build a runner; the loser
/// drops its half. That's cheaper than holding the pool lock across an
/// `await`, which would serialise all backend connects through one mutex.
pub async fn shared_runner(url: &str) -> Result<Arc<dyn QueryRunner>> {
    // Fast path: already cached.
    {
        let guard = pool().lock().expect("runner pool poisoned");
        if let Some(runner) = guard.get(url) {
            return Ok(Arc::clone(runner));
        }
    }
    // Slow path: build, then insert. If somebody else won the race we drop
    // ours and use theirs so callers see a single shared instance.
    let built = connect_runner(url).await?;
    let mut guard = pool().lock().expect("runner pool poisoned");
    let entry = guard
        .entry(url.to_string())
        .or_insert_with(|| Arc::clone(&built));
    Ok(Arc::clone(entry))
}

/// Drop every cached runner. Intended for tests that want a clean slate.
pub fn clear_pool_cache() {
    pool().lock().expect("runner pool poisoned").clear();
}

/// Current size of the runner pool. Intended for tests that want to assert
/// that the pool actually grew (or didn't).
pub fn pool_size() -> usize {
    pool().lock().expect("runner pool poisoned").len()
}
