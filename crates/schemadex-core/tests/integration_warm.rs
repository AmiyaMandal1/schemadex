//! Integration test for the `schemadex-warm` binary. Seeds an in-memory
//! SQLite db, invokes the binary with `--url` + `--cache-dir`, and asserts
//! that it exits 0 and prints the expected `warmed N tables` line.

#![cfg(feature = "sqlite")]

use sqlx::sqlite::SqlitePool;
use std::process::Command;

async fn seed(pool: &SqlitePool) {
    sqlx::query(
        "CREATE TABLE customers (id INTEGER PRIMARY KEY, email TEXT NOT NULL, region TEXT)",
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "CREATE TABLE orders (id INTEGER PRIMARY KEY, customer_id INTEGER NOT NULL REFERENCES customers(id), total INTEGER NOT NULL)",
    )
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn warm_builds_cache_for_sqlite() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let db_path = tmp.path().join("demo.sqlite");
    // sqlx-sqlite creates the file on connect when mode=rwc.
    let url = format!("sqlite://{}?mode=rwc", db_path.display());

    let pool = SqlitePool::connect(&url).await.unwrap();
    seed(&pool).await;
    pool.close().await;

    let cache_dir = tmp.path().join("cache");

    let output = Command::new(env!("CARGO_BIN_EXE_schemadex-warm"))
        .arg("--url")
        .arg(&url)
        .arg("--cache-dir")
        .arg(&cache_dir)
        .output()
        .expect("run schemadex-warm");

    assert!(
        output.status.success(),
        "binary exited with {:?}; stderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).expect("utf8 stdout");
    assert!(
        stdout.contains("warmed 2 tables"),
        "expected 'warmed 2 tables' in stdout, got:\n{stdout}"
    );
}
