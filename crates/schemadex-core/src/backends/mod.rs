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
pub async fn connect(url: &str) -> Result<Arc<dyn SchemaIntrospector>> {
    let scheme = url.split_once(':').map(|(s, _)| s).unwrap_or("");
    match scheme {
        #[cfg(feature = "postgres")]
        "postgres" | "postgresql" => Ok(Arc::new(
            postgres::PostgresIntrospector::connect(url).await?,
        )),
        #[cfg(feature = "sqlite")]
        "sqlite" | "file" => Ok(Arc::new(sqlite::SqliteIntrospector::connect(url).await?)),
        #[cfg(feature = "duckdb_backend")]
        "duckdb" => Ok(Arc::new(duckdb::DuckDbIntrospector::connect(url)?)),
        other => Err(SchemadexError::UnsupportedScheme(other.to_string())),
    }
}
