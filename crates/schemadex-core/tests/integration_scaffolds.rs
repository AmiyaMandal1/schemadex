//! Scaffold-backend integration tests.
//!
//! These verify the BigQuery / Snowflake / MSSQL stubs are wired into
//! the URL dispatcher and surface the expected polite "not yet implemented"
//! error rather than an `UnsupportedScheme`. No live network access is needed.

#![cfg(any(feature = "bigquery", feature = "snowflake", feature = "mssql"))]

use schemadex_core::backends;

async fn assert_unimpl(url: &str) {
    let introspector = backends::connect(url)
        .await
        .unwrap_or_else(|e| panic!("connect({url}) should dispatch to a scaffold, got: {e}"));
    let err = introspector
        .tables()
        .await
        .expect_err("scaffold tables() must return Err");
    let msg = err.to_string();
    assert!(
        msg.contains("not yet implemented"),
        "expected scaffold error from tables() at {url}, got: {msg}"
    );

    let runner = backends::connect_runner(url)
        .await
        .unwrap_or_else(|e| panic!("connect_runner({url}) should dispatch to a scaffold, got: {e}"));
    let err = runner
        .run_sql("SELECT 1", 10)
        .await
        .expect_err("scaffold run_sql() must return Err");
    let msg = err.to_string();
    assert!(
        msg.contains("not yet implemented"),
        "expected scaffold error from run_sql() at {url}, got: {msg}"
    );
}

#[cfg(feature = "bigquery")]
#[tokio::test]
async fn bigquery_scaffold_errors_politely() {
    assert_unimpl("bigquery://my-project/my_dataset").await;
}

#[cfg(feature = "snowflake")]
#[tokio::test]
async fn snowflake_scaffold_errors_politely() {
    assert_unimpl("snowflake://my-account/my_database/my_schema").await;
}

#[cfg(feature = "mssql")]
#[tokio::test]
async fn mssql_scaffold_errors_politely() {
    assert_unimpl("mssql://user:pass@localhost:1433/my_database").await;
}
