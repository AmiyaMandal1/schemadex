//! DuckDB backend via the synchronous `duckdb` crate. We wrap calls in
//! `tokio::task::spawn_blocking` to play nice with async callers.

use crate::error::{Result, SchemadexError};
use crate::introspector::{Backend, SchemaIntrospector};
use crate::model::{Column, DataType, ForeignKey, PrimaryKey};
use crate::sampling::{categorical_sample, numeric_sample, SamplingPolicy};
use async_trait::async_trait;
use duckdb::{params, Connection};
use std::sync::{Arc, Mutex};

pub struct DuckDbIntrospector {
    conn: Arc<Mutex<Connection>>,
    pub sampling: Option<SamplingPolicy>,
}

impl DuckDbIntrospector {
    pub fn connect(url: &str) -> Result<Self> {
        let path = url.trim_start_matches("duckdb://");
        let conn = if path.is_empty() || path == ":memory:" {
            Connection::open_in_memory()?
        } else {
            Connection::open(path)?
        };
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            sampling: None,
        })
    }

    pub fn with_sampling(mut self, policy: SamplingPolicy) -> Self {
        self.sampling = Some(policy);
        self
    }

    /// Sample one column. All DuckDB work runs inside a single
    /// `spawn_blocking` so the synchronous `Connection` doesn't end up on the
    /// async runtime thread. The closure returns the raw row data; the
    /// histogram/percentile assembly happens out here.
    async fn sample_column(
        &self,
        schema: &str,
        table: &str,
        col: &Column,
        policy: &SamplingPolicy,
    ) -> Result<Option<crate::model::ColumnSample>> {
        // Redaction check: skip sampling for likely-PII columns. We do this
        // before any DB round-trip so a redacted column also costs nothing.
        if policy
            .redaction
            .as_ref()
            .map(|r| r.should_redact(&col.name, col.comment.as_deref()))
            .unwrap_or(false)
        {
            tracing::debug!(column = %col.name, "duckdb.sample.redacted");
            return Ok(None);
        }

        let conn = self.conn.clone();
        let schema = schema.replace('"', "");
        let table = table.replace('"', "");
        let cname = col.name.replace('"', "");
        let qualified = format!("\"{schema}\".\"{table}\"");

        if col.data_type.is_numeric() {
            let limit = policy.sample_rows;
            let cname_q = cname.clone();
            let qualified_q = qualified.clone();
            let (values, null_count) = tokio::task::spawn_blocking(move || -> Result<(Vec<f64>, u64)> {
                let guard = conn
                    .lock()
                    .map_err(|_| SchemadexError::Other("duckdb lock poisoned".to_string()))?;
                let sample_sql = format!(
                    "SELECT CAST(\"{c}\" AS DOUBLE) AS v FROM {q} \
                     WHERE \"{c}\" IS NOT NULL LIMIT {limit}",
                    c = cname_q,
                    q = qualified_q,
                    limit = limit,
                );
                let mut stmt = guard.prepare(&sample_sql)?;
                let values: Vec<f64> = stmt
                    .query_map([], |r| r.get::<_, Option<f64>>(0))?
                    .filter_map(|r| r.ok().flatten())
                    .filter(|v| !v.is_nan())
                    .collect();
                let null_sql = format!(
                    "SELECT count(*) AS n FROM {q} WHERE \"{c}\" IS NULL",
                    q = qualified_q,
                    c = cname_q,
                );
                let null_count: i64 = guard
                    .query_row(&null_sql, [], |r| r.get::<_, i64>(0))
                    .unwrap_or(0);
                Ok((values, null_count.max(0) as u64))
            })
            .await
            .map_err(|e| SchemadexError::Other(format!("duckdb join: {e}")))??;
            let mut values = values;
            Ok(Some(numeric_sample(&mut values, null_count)))
        } else if col.data_type.is_categorical() {
            let topk = policy.top_k;
            let cname_q = cname.clone();
            let qualified_q = qualified.clone();
            let (top, null_count, distinct) = tokio::task::spawn_blocking(
                move || -> Result<(Vec<(String, u64)>, u64, Option<u64>)> {
                    let guard = conn
                        .lock()
                        .map_err(|_| SchemadexError::Other("duckdb lock poisoned".to_string()))?;
                    let topk_sql = format!(
                        "SELECT CAST(\"{c}\" AS VARCHAR) AS v, count(*) AS c FROM {q} \
                         WHERE \"{c}\" IS NOT NULL \
                         GROUP BY 1 ORDER BY count(*) DESC LIMIT {topk}",
                        c = cname_q,
                        q = qualified_q,
                        topk = topk,
                    );
                    let mut stmt = guard.prepare(&topk_sql)?;
                    let top: Vec<(String, u64)> = stmt
                        .query_map([], |r| {
                            Ok((
                                r.get::<_, Option<String>>(0)?,
                                r.get::<_, i64>(1)?,
                            ))
                        })?
                        .filter_map(|r| r.ok())
                        .filter_map(|(v, c)| v.map(|s| (s, c.max(0) as u64)))
                        .collect();
                    let null_sql = format!(
                        "SELECT count(*) AS n FROM {q} WHERE \"{c}\" IS NULL",
                        q = qualified_q,
                        c = cname_q,
                    );
                    let null_count: i64 = guard
                        .query_row(&null_sql, [], |r| r.get::<_, i64>(0))
                        .unwrap_or(0);
                    let distinct_sql = format!(
                        "SELECT count(DISTINCT \"{c}\") AS n FROM {q}",
                        c = cname_q,
                        q = qualified_q,
                    );
                    let distinct: Option<u64> = guard
                        .query_row(&distinct_sql, [], |r| r.get::<_, i64>(0))
                        .ok()
                        .map(|n| n.max(0) as u64);
                    Ok((top, null_count.max(0) as u64, distinct))
                },
            )
            .await
            .map_err(|e| SchemadexError::Other(format!("duckdb join: {e}")))??;
            let total_non_null: u64 = top.iter().map(|(_, c)| *c).sum();
            Ok(Some(categorical_sample(
                &top,
                total_non_null,
                null_count,
                distinct,
                policy,
            )))
        } else {
            Ok(None)
        }
    }
}

fn classify(decl: &str) -> DataType {
    let t = decl.to_uppercase();
    if t.starts_with("INT") || t == "BIGINT" || t == "SMALLINT" || t == "HUGEINT" || t == "UBIGINT"
    {
        DataType::Integer
    } else if t.starts_with("FLOAT") || t.starts_with("DOUBLE") || t == "REAL" {
        DataType::Float
    } else if t.starts_with("DECIMAL") || t.starts_with("NUMERIC") {
        DataType::Decimal
    } else if t == "VARCHAR" || t == "TEXT" || t.starts_with("CHAR") {
        DataType::Text
    } else if t == "BOOLEAN" {
        DataType::Bool
    } else if t == "DATE" {
        DataType::Date
    } else if t.contains("TIMESTAMP") {
        DataType::Timestamp
    } else if t.contains("TIME") {
        DataType::Time
    } else if t == "JSON" {
        DataType::Json
    } else if t == "UUID" {
        DataType::Uuid
    } else if t.contains("BLOB") {
        DataType::Bytes
    } else if t.contains("LIST") || t.contains("ARRAY") {
        DataType::Array
    } else {
        DataType::Unknown
    }
}

#[async_trait]
impl SchemaIntrospector for DuckDbIntrospector {
    fn backend(&self) -> Backend {
        Backend::DuckDb
    }

    #[tracing::instrument(level = "debug", name = "duckdb.tables", skip(self))]
    async fn tables(&self) -> Result<Vec<(Option<String>, String)>> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let guard = conn
                .lock()
                .map_err(|_| SchemadexError::Other("duckdb lock poisoned".to_string()))?;
            let mut stmt = guard.prepare(
                "SELECT table_schema, table_name FROM information_schema.tables \
                 WHERE table_type='BASE TABLE' AND table_schema NOT IN ('pg_catalog','information_schema') \
                 ORDER BY table_schema, table_name",
            )?;
            let rows = stmt
                .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            Ok(rows.into_iter().map(|(s, n)| (Some(s), n)).collect())
        })
        .await
        .map_err(|e| SchemadexError::Other(format!("duckdb join: {e}")))?
    }

    #[tracing::instrument(level = "debug", name = "duckdb.columns", skip(self))]
    async fn columns(&self, schema: Option<&str>, table: &str) -> Result<Vec<Column>> {
        let conn = self.conn.clone();
        let schema = schema
            .map(str::to_string)
            .unwrap_or_else(|| "main".to_string());
        let table = table.to_string();
        let schema_for_sample = schema.clone();
        let table_for_sample = table.clone();
        let mut cols: Vec<Column> = tokio::task::spawn_blocking(move || -> Result<Vec<Column>> {
            let guard = conn
                .lock()
                .map_err(|_| SchemadexError::Other("duckdb lock poisoned".to_string()))?;
            let mut stmt = guard.prepare(
                "SELECT column_name, data_type, is_nullable, column_default, ordinal_position \
                 FROM information_schema.columns \
                 WHERE table_schema = ? AND table_name = ? \
                 ORDER BY ordinal_position",
            )?;
            let rows = stmt
                .query_map(params![schema, table], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, Option<String>>(3)?,
                        r.get::<_, i32>(4)?,
                    ))
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            Ok(rows
                .into_iter()
                .map(|(name, native, nullable, default, ord)| Column {
                    name,
                    data_type: classify(&native),
                    native_type: native,
                    nullable: nullable.eq_ignore_ascii_case("YES"),
                    default,
                    comment: None,
                    ordinal: ord,
                    sample: None,
                })
                .collect())
        })
        .await
        .map_err(|e| SchemadexError::Other(format!("duckdb join: {e}")))??;

        if let Some(policy) = self.sampling.as_ref() {
            for col in cols.iter_mut() {
                let sample = self
                    .sample_column(&schema_for_sample, &table_for_sample, col, policy)
                    .await
                    .ok()
                    .flatten();
                col.sample = sample;
            }
        }
        Ok(cols)
    }

    #[tracing::instrument(level = "debug", name = "duckdb.primary_key", skip(self))]
    async fn primary_key(&self, schema: Option<&str>, table: &str) -> Result<Option<PrimaryKey>> {
        let conn = self.conn.clone();
        let schema = schema
            .map(str::to_string)
            .unwrap_or_else(|| "main".to_string());
        let table = table.to_string();
        tokio::task::spawn_blocking(move || {
            let guard = conn
                .lock()
                .map_err(|_| SchemadexError::Other("duckdb lock poisoned".to_string()))?;
            // `constraint_column_names` is a VARCHAR[]; serialize to a delimited
            // string so the duckdb crate gives us a single `String` back
            // (Vec<String> is not `FromSql`).
            let mut stmt = guard.prepare(
                "SELECT array_to_string(constraint_column_names, '\u{1f}') \
                 FROM duckdb_constraints() \
                 WHERE schema_name = ? AND table_name = ? AND constraint_type = 'PRIMARY KEY'",
            )?;
            let rows = stmt
                .query_map(params![schema, table], |r| r.get::<_, Option<String>>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            let joined: Option<String> = rows.into_iter().flatten().next();
            match joined {
                Some(s) if !s.is_empty() => Ok(Some(PrimaryKey {
                    name: None,
                    columns: s.split('\u{1f}').map(str::to_string).collect(),
                })),
                _ => Ok(None),
            }
        })
        .await
        .map_err(|e| SchemadexError::Other(format!("duckdb join: {e}")))?
    }

    #[tracing::instrument(level = "debug", name = "duckdb.foreign_keys", skip(self))]
    async fn foreign_keys(&self, schema: Option<&str>, table: &str) -> Result<Vec<ForeignKey>> {
        let conn = self.conn.clone();
        let schema = schema
            .map(str::to_string)
            .unwrap_or_else(|| "main".to_string());
        let table = table.to_string();
        tokio::task::spawn_blocking(move || {
            let guard = conn
                .lock()
                .map_err(|_| SchemadexError::Other("duckdb lock poisoned".to_string()))?;
            let mut stmt = guard.prepare(
                "SELECT constraint_name, \
                        array_to_string(constraint_column_names, '\u{1f}'), \
                        referenced_table, \
                        array_to_string(referenced_column_names, '\u{1f}') \
                 FROM duckdb_constraints() \
                 WHERE schema_name = ? AND table_name = ? AND constraint_type = 'FOREIGN KEY'",
            )?;
            let rows = stmt
                .query_map(params![schema, table], |r| {
                    Ok((
                        r.get::<_, Option<String>>(0)?,
                        r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                        r.get::<_, String>(2)?,
                        r.get::<_, Option<String>>(3)?.unwrap_or_default(),
                    ))
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            Ok(rows
                .into_iter()
                .map(|(name, columns_joined, referenced_table, referenced_columns_joined)| {
                    let columns = columns_joined
                        .split('\u{1f}')
                        .filter(|s| !s.is_empty())
                        .map(str::to_string)
                        .collect();
                    let referenced_columns = referenced_columns_joined
                        .split('\u{1f}')
                        .filter(|s| !s.is_empty())
                        .map(str::to_string)
                        .collect();
                    ForeignKey {
                        name,
                        columns,
                        referenced_table,
                        referenced_columns,
                    }
                })
                .collect())
        })
        .await
        .map_err(|e| SchemadexError::Other(format!("duckdb join: {e}")))?
    }
}
