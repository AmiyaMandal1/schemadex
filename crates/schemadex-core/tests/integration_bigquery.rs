//! Integration test for the BigQuery backend.
//!
//! Gated on `DATABASE_URL_BIGQUERY` *and* `GOOGLE_APPLICATION_CREDENTIALS`.
//! If either is missing we eprintln-skip — CI without GCP credentials should
//! see this as a pass, not a failure.

#![cfg(feature = "bigquery")]

use schemadex_core::backends::bigquery::BigQueryIntrospector;
use schemadex_core::introspector::SchemaIntrospector;
use schemadex_core::QueryRunner;

#[tokio::test]
async fn bigquery_smoke() {
    let url = match std::env::var("DATABASE_URL_BIGQUERY") {
        Ok(u) if !u.is_empty() => u,
        _ => {
            eprintln!("skipping bigquery tests: DATABASE_URL_BIGQUERY not set");
            return;
        }
    };
    if std::env::var("GOOGLE_APPLICATION_CREDENTIALS")
        .ok()
        .filter(|s| !s.is_empty())
        .is_none()
    {
        eprintln!(
            "skipping bigquery tests: GOOGLE_APPLICATION_CREDENTIALS not set"
        );
        return;
    }

    let introspector = BigQueryIntrospector::connect(&url)
        .await
        .expect("connect to bigquery");

    // tables() should succeed (the dataset may be empty, that's fine).
    let tables = introspector.tables().await.expect("list tables");
    let _: &Vec<(Option<String>, String)> = &tables;

    // run_sql with a literal that does not depend on any user data.
    let result = introspector
        .run_sql("SELECT 1 AS v", 10)
        .await
        .expect("run SELECT 1");
    assert_eq!(result.columns, vec!["v".to_string()]);
    assert_eq!(result.rows.len(), 1);
}
