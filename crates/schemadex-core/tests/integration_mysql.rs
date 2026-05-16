//! Integration test for the MySQL backend. Requires both:
//!   - `--features mysql`
//!   - the `DATABASE_URL_MYSQL` env var pointing at a reachable MySQL/MariaDB
//!
//! When the env var is absent we print a skip notice and return early — CI
//! that doesn't provision a MySQL instance should see this as a passing test,
//! not a missing connection failure.

#![cfg(feature = "mysql")]

use schemadex_core::backends::mysql::MysqlIntrospector;
use schemadex_core::introspector::SchemaIntrospector;
use schemadex_core::QueryRunner;

#[tokio::test]
async fn mysql_smoke() {
    let url = match std::env::var("DATABASE_URL_MYSQL") {
        Ok(u) if !u.is_empty() => u,
        _ => {
            eprintln!("skipping mysql tests: DATABASE_URL_MYSQL not set");
            return;
        }
    };

    let introspector = MysqlIntrospector::connect(&url)
        .await
        .expect("connect to mysql");

    // tables() may return an empty Vec on a fresh database; we only care that
    // the call succeeds and produces a Vec.
    let tables = introspector.tables().await.expect("list tables");
    let _: &Vec<(Option<String>, String)> = &tables;

    // run_sql("SELECT 1") should give us a single-column result regardless of
    // the database state.
    let result = introspector
        .run_sql("SELECT 1", 10)
        .await
        .expect("run SELECT 1");
    assert_eq!(result.columns.len(), 1);
}
