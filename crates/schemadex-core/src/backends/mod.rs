//! Backend implementations. Each backend lives behind a feature flag so
//! a thin wheel can ship one driver instead of all three.

#[cfg(feature = "postgres")]
pub mod postgres;

#[cfg(feature = "sqlite")]
pub mod sqlite;

#[cfg(feature = "duckdb_backend")]
pub mod duckdb;

use crate::error::{Result, SchemadexError};
use crate::introspector::SchemaIntrospector;
use std::sync::Arc;

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
/// Currently only the postgres backend honors `sampling`; sqlite and duckdb
/// silently ignore it.
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
        // TODO: thread `sampling` into the sqlite backend once it supports
        // sample-value collection.
        "sqlite" | "file" => {
            let _ = sampling;
            Ok(Arc::new(sqlite::SqliteIntrospector::connect(url).await?))
        }
        #[cfg(feature = "duckdb_backend")]
        // TODO: thread `sampling` into the duckdb backend once it supports
        // sample-value collection.
        "duckdb" => {
            let _ = sampling;
            Ok(Arc::new(duckdb::DuckDbIntrospector::connect(url)?))
        }
        other => Err(SchemadexError::UnsupportedScheme(other.to_string())),
    }
}
