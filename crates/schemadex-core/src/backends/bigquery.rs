//! BigQuery backend (v0.5 scaffold).
//!
//! This file ships the trait skeleton so the URL scheme dispatches to a
//! BigQuery-specific entry point. The actual query path is unimplemented;
//! call sites get a clear `Other(...)` error instead of `UnsupportedScheme`.
//! Production support will land alongside `gcp-bigquery-client` integration.

use crate::error::{Result, SchemadexError};
use crate::introspector::{Backend, QueryResult, QueryRunner, SchemaIntrospector};
use crate::model::{Column, ForeignKey, PrimaryKey};
use async_trait::async_trait;

pub struct BigQueryIntrospector {
    pub project: String,
    pub dataset: Option<String>,
}

impl BigQueryIntrospector {
    pub fn connect(url: &str) -> Result<Self> {
        // bigquery://project/dataset
        let trimmed = url.trim_start_matches("bigquery://");
        let mut parts = trimmed.splitn(2, '/');
        let project = parts.next().unwrap_or("").to_string();
        let dataset = parts.next().map(str::to_string);
        if project.is_empty() {
            return Err(SchemadexError::Other(
                "BigQuery URL must include a project: bigquery://project[/dataset]".into(),
            ));
        }
        Ok(Self { project, dataset })
    }
}

fn unimpl<T>(what: &str) -> Result<T> {
    Err(SchemadexError::Other(format!(
        "BigQuery {what} not yet implemented (v0.5 scaffold)"
    )))
}

#[async_trait]
impl SchemaIntrospector for BigQueryIntrospector {
    fn backend(&self) -> Backend {
        Backend::BigQuery
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
impl QueryRunner for BigQueryIntrospector {
    async fn run_sql(&self, _: &str, _: usize) -> Result<QueryResult> {
        unimpl("run_sql")
    }
}
