//! Snowflake backend (v0.5 scaffold).
//!
//! This file ships the trait skeleton so the URL scheme dispatches to a
//! Snowflake-specific entry point. The actual query path is unimplemented;
//! call sites get a clear `Other(...)` error instead of `UnsupportedScheme`.
//! Production support will land alongside a Snowflake SDK integration.

use crate::error::{Result, SchemadexError};
use crate::introspector::{Backend, QueryResult, QueryRunner, SchemaIntrospector};
use crate::model::{Column, ForeignKey, PrimaryKey};
use async_trait::async_trait;

pub struct SnowflakeIntrospector {
    pub account: String,
    pub database: String,
    pub schema: Option<String>,
}

impl SnowflakeIntrospector {
    pub fn connect(url: &str) -> Result<Self> {
        // snowflake://account/database[/schema]
        let trimmed = url.trim_start_matches("snowflake://");
        let mut parts = trimmed.splitn(3, '/');
        let account = parts.next().unwrap_or("").to_string();
        let database = parts.next().unwrap_or("").to_string();
        let schema = parts.next().map(str::to_string);
        if account.is_empty() || database.is_empty() {
            return Err(SchemadexError::Other(
                "Snowflake URL must include an account and database: \
                 snowflake://account/database[/schema]"
                    .into(),
            ));
        }
        Ok(Self {
            account,
            database,
            schema,
        })
    }
}

fn unimpl<T>(what: &str) -> Result<T> {
    Err(SchemadexError::Other(format!(
        "Snowflake {what} not yet implemented (v0.5 scaffold)"
    )))
}

#[async_trait]
impl SchemaIntrospector for SnowflakeIntrospector {
    fn backend(&self) -> Backend {
        Backend::Snowflake
    }
    async fn tables(&self) -> Result<Vec<(Option<String>, String)>> {
        unimpl("tables")
    }
    async fn columns(&self, _: Option<&str>, _: &str) -> Result<Vec<Column>> {
        unimpl("columns")
    }
    async fn primary_key(&self, _: Option<&str>, _: &str) -> Result<Option<PrimaryKey>> {
        unimpl("primary_key")
    }
    async fn foreign_keys(&self, _: Option<&str>, _: &str) -> Result<Vec<ForeignKey>> {
        unimpl("foreign_keys")
    }
}

#[async_trait]
impl QueryRunner for SnowflakeIntrospector {
    async fn run_sql(&self, _: &str, _: usize) -> Result<QueryResult> {
        unimpl("run_sql")
    }
}
