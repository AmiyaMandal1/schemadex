//! Integration test against an in-process SQLite database. Confirms the
//! trait abstraction works end-to-end without any backend-specific branching
//! in cache code.

#![cfg(feature = "sqlite")]

use schemadex_core::backends::sqlite::SqliteIntrospector;
use schemadex_core::cache::{CacheOptions, SchemaCache};
use sqlx::sqlite::SqlitePool;
use std::time::Duration;

async fn seed(pool: &SqlitePool) {
    sqlx::query(
        "CREATE TABLE customers (id INTEGER PRIMARY KEY, email TEXT NOT NULL, region TEXT)",
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query("CREATE TABLE orders (id INTEGER PRIMARY KEY, customer_id INTEGER NOT NULL REFERENCES customers(id), total INTEGER NOT NULL)")
        .execute(pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn cache_round_trip() {
    let tmp = tempdir_or_skip();
    let db_path = tmp.path().join("demo.sqlite");
    // sqlx-sqlite creates the file on connect when mode=rwc.
    let url = format!("sqlite://{}?mode=rwc", db_path.display());

    let pool = SqlitePool::connect(&url).await.unwrap();
    seed(&pool).await;
    pool.close().await;

    let introspector = SqliteIntrospector::connect(&url).await.unwrap();
    let opts = CacheOptions {
        ttl: Duration::from_secs(60),
        cache_dir: Some(tmp.path().join("cache")),
        parallel: true,
        ..Default::default()
    };

    let cache = SchemaCache::from_introspector(&introspector, &url, &opts)
        .await
        .unwrap();
    let names = cache.database().list_tables();
    assert!(names.contains(&"customers".to_string()));
    assert!(names.contains(&"orders".to_string()));

    let orders = cache.database().table("orders").expect("orders table");
    assert!(orders
        .foreign_keys
        .iter()
        .any(|fk| fk.referenced_table == "customers"));
}

fn tempdir_or_skip() -> tempfile::TempDir {
    tempfile::tempdir().expect("temp dir")
}
