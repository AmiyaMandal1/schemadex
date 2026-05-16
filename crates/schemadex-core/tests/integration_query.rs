//! End-to-end test for the `QueryRunner` trait against in-process SQLite.

#![cfg(feature = "sqlite")]

use schemadex_core::backends::sqlite::SqliteIntrospector;
use schemadex_core::QueryRunner;
use sqlx::sqlite::SqlitePool;

async fn seed(pool: &SqlitePool) {
    sqlx::query("CREATE TABLE customers (id INTEGER PRIMARY KEY, email TEXT NOT NULL)")
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO customers (email) VALUES ('a@x.com'), ('b@x.com')")
        .execute(pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn run_sql_returns_rows_and_columns() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let db_path = tmp.path().join("demo.sqlite");
    let url = format!("sqlite://{}?mode=rwc", db_path.display());

    let pool = SqlitePool::connect(&url).await.unwrap();
    seed(&pool).await;
    pool.close().await;

    let introspector = SqliteIntrospector::connect(&url).await.unwrap();
    let result = introspector
        .run_sql("SELECT id, email FROM customers ORDER BY id", 10)
        .await
        .unwrap();

    assert_eq!(result.columns, vec!["id".to_string(), "email".to_string()]);
    assert_eq!(result.rows.len(), 2);
    assert!(!result.truncated);
    // Cell stringification: id should be a parseable number, email should be
    // the raw text.
    assert!(result.rows[0][1].contains('@'));
}

#[tokio::test]
async fn run_sql_flags_truncation() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let db_path = tmp.path().join("trunc.sqlite");
    let url = format!("sqlite://{}?mode=rwc", db_path.display());

    let pool = SqlitePool::connect(&url).await.unwrap();
    seed(&pool).await;
    pool.close().await;

    let introspector = SqliteIntrospector::connect(&url).await.unwrap();
    let result = introspector
        .run_sql("SELECT id, email FROM customers ORDER BY id", 1)
        .await
        .unwrap();

    assert_eq!(result.rows.len(), 1);
    assert!(result.truncated);
}
