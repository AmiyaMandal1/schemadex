use crate::error::Result;
use crate::model::{Column, ForeignKey, PrimaryKey, Table};
use async_trait::async_trait;

/// Result of executing a SELECT through a [`QueryRunner`].
///
/// Every cell is stringified — this is an agent-ergonomic API meant to feed
/// LLM prompts, not an OLAP path. The `truncated` flag is set by the runner
/// when it detected at least one more row beyond `row_limit` (it fetches
/// `row_limit + 1` and trims).
#[derive(Debug, Clone)]
pub struct QueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<String>>,
    pub truncated: bool,
}

/// Execute ad-hoc SELECTs against a live backend. Implementations live
/// alongside the matching [`SchemaIntrospector`] so a single connection pool
/// powers both schema introspection and result fetching.
#[async_trait]
pub trait QueryRunner: Send + Sync {
    /// Execute `sql` and return up to `row_limit` rows as a homogeneous
    /// table of strings (one row per inner `Vec`).
    async fn run_sql(&self, sql: &str, row_limit: usize) -> Result<QueryResult>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Backend {
    Postgres,
    Sqlite,
    Mysql,
    DuckDb,
    BigQuery,
    Snowflake,
    Mssql,
}

impl Backend {
    pub fn as_str(self) -> &'static str {
        match self {
            Backend::Postgres => "postgres",
            Backend::Sqlite => "sqlite",
            Backend::Mysql => "mysql",
            Backend::DuckDb => "duckdb",
            Backend::BigQuery => "bigquery",
            Backend::Snowflake => "snowflake",
            Backend::Mssql => "mssql",
        }
    }
}

/// Live database introspection. Implementations live in [`crate::backends`].
///
/// All methods take the qualified or bare table name as understood by the
/// backend; the introspector is responsible for resolving the default schema.
#[async_trait]
pub trait SchemaIntrospector: Send + Sync {
    fn backend(&self) -> Backend;

    async fn tables(&self) -> Result<Vec<(Option<String>, String)>>;

    async fn columns(&self, schema: Option<&str>, table: &str) -> Result<Vec<Column>>;

    async fn primary_key(&self, schema: Option<&str>, table: &str) -> Result<Option<PrimaryKey>>;

    async fn foreign_keys(&self, schema: Option<&str>, table: &str) -> Result<Vec<ForeignKey>>;

    async fn row_count_estimate(&self, _schema: Option<&str>, _table: &str) -> Result<Option<u64>> {
        Ok(None)
    }

    async fn table_comment(&self, _schema: Option<&str>, _table: &str) -> Result<Option<String>> {
        Ok(None)
    }

    /// Backend-specific DDL/identity string used for fingerprinting.
    async fn ddl_signature(&self, schema: Option<&str>, table: &str) -> Result<String> {
        let cols = self.columns(schema, table).await?;
        let pk = self.primary_key(schema, table).await?;
        let fks = self.foreign_keys(schema, table).await?;
        let payload = serde_json::json!({
            "schema": schema,
            "table": table,
            "cols": cols,
            "pk": pk,
            "fks": fks,
        });
        Ok(payload.to_string())
    }

    /// Default refresh path: list tables, then fan out per-table introspection.
    async fn introspect_table(&self, schema: Option<&str>, name: &str) -> Result<Table> {
        let columns = self.columns(schema, name).await?;
        let primary_key = self.primary_key(schema, name).await?;
        let foreign_keys = self.foreign_keys(schema, name).await?;
        let row_count_estimate = self.row_count_estimate(schema, name).await?;
        let comment = self.table_comment(schema, name).await?;
        Ok(Table {
            schema: schema.map(str::to_string),
            name: name.to_string(),
            comment,
            columns,
            primary_key,
            foreign_keys,
            row_count_estimate,
            ddl_hash: None,
        })
    }
}
