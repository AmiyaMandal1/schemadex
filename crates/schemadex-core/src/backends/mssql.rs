//! Microsoft SQL Server backend (v0.5 scaffold).
//!
//! This file ships the trait skeleton so the URL scheme dispatches to an
//! MSSQL-specific entry point. The actual query path is unimplemented;
//! call sites get a clear `Other(...)` error instead of `UnsupportedScheme`.
//! Production support will land alongside a `tiberius`-based driver
//! integration.

use crate::error::{Result, SchemadexError};
use crate::introspector::{Backend, QueryResult, QueryRunner, SchemaIntrospector};
use crate::model::{Column, ForeignKey, PrimaryKey};
use async_trait::async_trait;

pub struct MssqlIntrospector {
    pub url: String,
}

impl MssqlIntrospector {
    pub fn connect(url: &str) -> Result<Self> {
        // mssql://user:pass@host:port/database  (or sqlserver://)
        let stripped = url
            .strip_prefix("mssql://")
            .or_else(|| url.strip_prefix("sqlserver://"))
            .unwrap_or(url);
        if stripped.is_empty() || !stripped.contains('/') {
            return Err(SchemadexError::Other(
                "MSSQL URL must include host and database: \
                 mssql://user:pass@host:port/database"
                    .into(),
            ));
        }
        Ok(Self {
            url: url.to_string(),
        })
    }
}

fn unimpl<T>(what: &str) -> Result<T> {
    Err(SchemadexError::Other(format!(
        "MSSQL {what} not yet implemented (v0.5 scaffold)"
    )))
}

#[async_trait]
impl SchemaIntrospector for MssqlIntrospector {
    fn backend(&self) -> Backend {
        Backend::Mssql
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
impl QueryRunner for MssqlIntrospector {
    async fn run_sql(&self, _: &str, _: usize) -> Result<QueryResult> {
        unimpl("run_sql")
    }
}
