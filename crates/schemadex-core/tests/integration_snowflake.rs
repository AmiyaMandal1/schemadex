//! Integration test for the Snowflake backend.
//!
//! Gated on `DATABASE_URL_SNOWFLAKE` *and* `SNOWFLAKE_PRIVATE_KEY_PATH`. If
//! either is missing we eprintln-skip — CI without Snowflake credentials
//! should treat this as a pass.

#![cfg(feature = "snowflake")]

use schemadex_core::backends::snowflake::SnowflakeIntrospector;
use schemadex_core::introspector::SchemaIntrospector;
use schemadex_core::QueryRunner;

#[tokio::test]
async fn snowflake_smoke() {
    let url = match std::env::var("DATABASE_URL_SNOWFLAKE") {
        Ok(u) if !u.is_empty() => u,
        _ => {
            eprintln!("skipping snowflake tests: DATABASE_URL_SNOWFLAKE not set");
            return;
        }
    };
    if std::env::var("SNOWFLAKE_PRIVATE_KEY_PATH")
        .ok()
        .filter(|s| !s.is_empty())
        .is_none()
    {
        eprintln!("skipping snowflake tests: SNOWFLAKE_PRIVATE_KEY_PATH not set");
        return;
    }

    let introspector = SnowflakeIntrospector::connect(&url)
        .await
        .expect("connect to snowflake");

    // tables() should round-trip even on an empty database.
    let _ = introspector.tables().await.expect("list tables");

    let result = introspector
        .run_sql("SELECT 1 AS V", 10)
        .await
        .expect("run SELECT 1");
    assert!(!result.columns.is_empty(), "expected at least one column");
}
