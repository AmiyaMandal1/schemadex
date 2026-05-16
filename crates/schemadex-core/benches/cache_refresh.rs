//! Cold-vs-warm cache benchmark. Seeds a SQLite database with 50 tables,
//! introspects once to populate the disk cache, then measures the time to
//! load the same cache.
//!
//! Run with `cargo bench -p schemadex-core --features sqlite --bench cache_refresh`.
//! Requires the `sqlite` feature.

#![cfg(feature = "sqlite")]

use std::time::{Duration, Instant};

use schemadex_core::backends::sqlite::SqliteIntrospector;
use schemadex_core::cache::{CacheOptions, SchemaCache};
use sqlx::sqlite::SqlitePool;

#[tokio::main]
async fn main() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let db = tmp.path().join("bench.sqlite");
    let url = format!("sqlite://{}?mode=rwc", db.display());

    seed(&url, 50).await;

    let introspector = SqliteIntrospector::connect(&url).await.unwrap();
    let opts = CacheOptions {
        ttl: Duration::from_secs(3600),
        cache_dir: Some(tmp.path().join("cache")),
        parallel: true,
        ..Default::default()
    };

    // Cold: no cache file yet.
    let cold_start = Instant::now();
    let cold = SchemaCache::from_introspector(&introspector, &url, &opts)
        .await
        .unwrap();
    let cold_elapsed = cold_start.elapsed();
    drop(cold);

    // Warm: same call should hit the on-disk cache.
    let warm_start = Instant::now();
    let warm = SchemaCache::from_introspector(&introspector, &url, &opts)
        .await
        .unwrap();
    let warm_elapsed = warm_start.elapsed();

    let speedup = cold_elapsed.as_secs_f64() / warm_elapsed.as_secs_f64().max(1e-9);
    println!(
        "tables: {}  cold: {:?}  warm: {:?}  speedup: {:.1}x",
        warm.database().tables.len(),
        cold_elapsed,
        warm_elapsed,
        speedup,
    );
}

async fn seed(url: &str, n: usize) {
    let pool = SqlitePool::connect(url).await.unwrap();
    for i in 0..n {
        let ddl = format!(
            "CREATE TABLE t_{i} (id INTEGER PRIMARY KEY, payload TEXT NOT NULL, created_at TEXT)"
        );
        sqlx::query(&ddl).execute(&pool).await.unwrap();
    }
    pool.close().await;
}
