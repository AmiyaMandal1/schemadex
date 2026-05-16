//! SQLite backend via sqlx. SQLite has no schemas (well, `main` and attached
//! databases — we ignore attachments by default).

use crate::error::Result;
use crate::introspector::{Backend, QueryResult, QueryRunner, SchemaIntrospector};
use crate::model::{Column, DataType, ForeignKey, PrimaryKey};
use crate::sampling::{categorical_sample, numeric_sample, SamplingPolicy};
use async_trait::async_trait;
use sqlx::sqlite::{SqlitePool, SqlitePoolOptions};
use sqlx::{Column as _, Row, TypeInfo, ValueRef};

pub struct SqliteIntrospector {
    pool: SqlitePool,
    pub sampling: Option<SamplingPolicy>,
}

impl SqliteIntrospector {
    pub async fn connect(url: &str) -> Result<Self> {
        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect(url)
            .await?;
        Ok(Self {
            pool,
            sampling: None,
        })
    }

    pub fn with_sampling(mut self, policy: SamplingPolicy) -> Self {
        self.sampling = Some(policy);
        self
    }

    async fn sample_column(
        &self,
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
            tracing::debug!(column = %col.name, "sqlite.sample.redacted");
            return Ok(None);
        }
        let t = table.replace('"', "");
        let c = col.name.replace('"', "");
        if col.data_type.is_numeric() {
            let sql = format!(
                "SELECT CAST(\"{c}\" AS REAL) AS v FROM \"{t}\" \
                 WHERE \"{c}\" IS NOT NULL LIMIT {limit}",
                c = c,
                t = t,
                limit = policy.sample_rows,
            );
            let rows = sqlx::query(&sql).fetch_all(&self.pool).await?;
            let mut values: Vec<f64> = rows
                .iter()
                .map(|r| r.try_get::<f64, _>("v").unwrap_or(f64::NAN))
                .filter(|v| !v.is_nan())
                .collect();
            let null_sql = format!(
                "SELECT count(*) AS n FROM \"{t}\" WHERE \"{c}\" IS NULL",
                t = t,
                c = c,
            );
            let null_count: i64 = sqlx::query(&null_sql)
                .fetch_one(&self.pool)
                .await
                .ok()
                .and_then(|r| r.try_get::<i64, _>("n").ok())
                .unwrap_or(0);
            Ok(Some(numeric_sample(&mut values, null_count.max(0) as u64)))
        } else if col.data_type.is_categorical() {
            let sql = format!(
                "SELECT \"{c}\" AS v, count(*) AS c FROM \"{t}\" \
                 WHERE \"{c}\" IS NOT NULL \
                 GROUP BY 1 ORDER BY count(*) DESC LIMIT {topk}",
                c = c,
                t = t,
                topk = policy.top_k,
            );
            let rows = sqlx::query(&sql).fetch_all(&self.pool).await?;
            let top: Vec<(String, u64)> = rows
                .iter()
                .filter_map(|r| {
                    let v: Option<String> = r.try_get("v").ok();
                    let c: Option<i64> = r.try_get("c").ok();
                    Some((v?, c?.max(0) as u64))
                })
                .collect();
            let total_non_null: u64 = top.iter().map(|(_, c)| *c).sum();
            let null_sql = format!(
                "SELECT count(*) AS n FROM \"{t}\" WHERE \"{c}\" IS NULL",
                t = t,
                c = c,
            );
            let null_count: i64 = sqlx::query(&null_sql)
                .fetch_one(&self.pool)
                .await
                .ok()
                .and_then(|r| r.try_get::<i64, _>("n").ok())
                .unwrap_or(0);
            let distinct_sql = format!(
                "SELECT count(DISTINCT \"{c}\") AS n FROM \"{t}\"",
                c = c,
                t = t,
            );
            let distinct: Option<u64> = sqlx::query(&distinct_sql)
                .fetch_one(&self.pool)
                .await
                .ok()
                .and_then(|r| r.try_get::<i64, _>("n").ok())
                .map(|n| n.max(0) as u64);
            Ok(Some(categorical_sample(
                &top,
                total_non_null,
                null_count.max(0) as u64,
                distinct,
                policy,
            )))
        } else {
            Ok(None)
        }
    }
}

fn classify(decl: &str) -> DataType {
    let t = decl.to_lowercase();
    if t.contains("int") {
        DataType::Integer
    } else if t.contains("real") || t.contains("floa") || t.contains("doub") {
        DataType::Float
    } else if t.contains("num") || t.contains("dec") {
        DataType::Decimal
    } else if t.contains("char") || t.contains("text") || t.contains("clob") {
        DataType::Text
    } else if t.contains("bool") {
        DataType::Bool
    } else if t.contains("date") && !t.contains("time") {
        DataType::Date
    } else if t.contains("time") {
        DataType::Timestamp
    } else if t.contains("blob") {
        DataType::Bytes
    } else {
        DataType::Unknown
    }
}

#[async_trait]
impl SchemaIntrospector for SqliteIntrospector {
    fn backend(&self) -> Backend {
        Backend::Sqlite
    }

    #[tracing::instrument(level = "debug", name = "sqlite.tables", skip(self))]
    async fn tables(&self) -> Result<Vec<(Option<String>, String)>> {
        let rows = sqlx::query(
            "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| (None, r.try_get::<String, _>("name").unwrap_or_default()))
            .collect())
    }

    #[tracing::instrument(level = "debug", name = "sqlite.columns", skip(self))]
    async fn columns(&self, _schema: Option<&str>, table: &str) -> Result<Vec<Column>> {
        let sql = format!("PRAGMA table_info(\"{}\")", table.replace('"', ""));
        let rows = sqlx::query(&sql).fetch_all(&self.pool).await?;
        let mut cols = Vec::with_capacity(rows.len());
        for r in rows {
            let ordinal: i64 = r.try_get("cid").unwrap_or(0);
            let name: String = r.try_get("name")?;
            let decl: String = r.try_get("type")?;
            let nullable_flag: i64 = r.try_get("notnull").unwrap_or(0);
            let default: Option<String> = r.try_get("dflt_value").ok();
            cols.push(Column {
                name,
                data_type: classify(&decl),
                native_type: decl,
                nullable: nullable_flag == 0,
                default,
                comment: None,
                ordinal: ordinal as i32,
                sample: None,
            });
        }

        if let Some(policy) = self.sampling.as_ref() {
            for col in cols.iter_mut() {
                let sample = self
                    .sample_column(table, col, policy)
                    .await
                    .ok()
                    .flatten();
                col.sample = sample;
            }
        }
        Ok(cols)
    }

    #[tracing::instrument(level = "debug", name = "sqlite.primary_key", skip(self))]
    async fn primary_key(&self, _schema: Option<&str>, table: &str) -> Result<Option<PrimaryKey>> {
        let sql = format!("PRAGMA table_info(\"{}\")", table.replace('"', ""));
        let rows = sqlx::query(&sql).fetch_all(&self.pool).await?;
        let mut pk_cols: Vec<(i64, String)> = rows
            .iter()
            .filter_map(|r| {
                let order: i64 = r.try_get("pk").unwrap_or(0);
                if order > 0 {
                    Some((order, r.try_get::<String, _>("name").ok()?))
                } else {
                    None
                }
            })
            .collect();
        if pk_cols.is_empty() {
            return Ok(None);
        }
        pk_cols.sort_by_key(|(o, _)| *o);
        Ok(Some(PrimaryKey {
            name: None,
            columns: pk_cols.into_iter().map(|(_, n)| n).collect(),
        }))
    }

    #[tracing::instrument(level = "debug", name = "sqlite.foreign_keys", skip(self))]
    async fn foreign_keys(&self, _schema: Option<&str>, table: &str) -> Result<Vec<ForeignKey>> {
        let sql = format!("PRAGMA foreign_key_list(\"{}\")", table.replace('"', ""));
        let rows = sqlx::query(&sql).fetch_all(&self.pool).await?;
        use std::collections::BTreeMap;
        let mut by_id: BTreeMap<i64, ForeignKey> = BTreeMap::new();
        for r in rows {
            let id: i64 = r.try_get("id").unwrap_or(0);
            let ref_table: String = r.try_get("table").unwrap_or_default();
            let from: String = r.try_get("from").unwrap_or_default();
            let to: String = r.try_get("to").unwrap_or_default();
            let fk = by_id.entry(id).or_insert(ForeignKey {
                name: None,
                columns: vec![],
                referenced_table: ref_table,
                referenced_columns: vec![],
            });
            fk.columns.push(from);
            fk.referenced_columns.push(to);
        }
        Ok(by_id.into_values().collect())
    }
}

/// Render a sqlite cell as a string. The driver returns typed values so we
/// peek at the type and coerce. NULL becomes the empty string — the agent
/// reads markdown, not three-valued logic.
fn sqlite_cell_to_string(row: &sqlx::sqlite::SqliteRow, idx: usize) -> String {
    let raw = match row.try_get_raw(idx) {
        Ok(r) => r,
        Err(_) => return String::new(),
    };
    if raw.is_null() {
        return String::new();
    }
    let type_name = raw.type_info().name().to_ascii_uppercase();
    match type_name.as_str() {
        "INTEGER" | "INT" | "BIGINT" => row
            .try_get::<i64, _>(idx)
            .map(|v| v.to_string())
            .unwrap_or_default(),
        "REAL" | "FLOAT" | "DOUBLE" | "NUMERIC" => row
            .try_get::<f64, _>(idx)
            .map(|v| v.to_string())
            .unwrap_or_default(),
        "BOOLEAN" => row
            .try_get::<bool, _>(idx)
            .map(|v| v.to_string())
            .unwrap_or_default(),
        "BLOB" => row
            .try_get::<Vec<u8>, _>(idx)
            .map(|v| format!("<{} bytes>", v.len()))
            .unwrap_or_default(),
        // TEXT and the catch-all "NULL" sqlite type (when the column has no
        // affinity and the value was bound as text).
        _ => row.try_get::<String, _>(idx).unwrap_or_default(),
    }
}

#[async_trait]
impl QueryRunner for SqliteIntrospector {
    #[tracing::instrument(
        level = "debug",
        name = "sqlite.run_sql",
        skip(self, sql),
        fields(sql_len = sql.len(), row_limit),
    )]
    async fn run_sql(&self, sql: &str, row_limit: usize) -> Result<QueryResult> {
        // Fetch one extra row so we can flag truncation without a second
        // round-trip. We don't try to wrap the user's SQL in a subquery —
        // sqlite isn't going to materialize a billion rows behind a LIMIT we
        // tacked on, and rewriting arbitrary SELECT statements is fragile.
        let cap = row_limit.saturating_add(1);
        let rows = sqlx::query(sql).fetch_all(&self.pool).await?;
        let truncated = rows.len() > row_limit;
        let take = rows.len().min(row_limit).min(cap);

        let columns: Vec<String> = rows
            .first()
            .map(|r| r.columns().iter().map(|c| c.name().to_string()).collect())
            .unwrap_or_default();

        let mut out_rows = Vec::with_capacity(take);
        for r in rows.iter().take(take) {
            let mut row = Vec::with_capacity(columns.len());
            for i in 0..r.columns().len() {
                row.push(sqlite_cell_to_string(r, i));
            }
            out_rows.push(row);
        }

        Ok(QueryResult {
            columns,
            rows: out_rows,
            truncated,
        })
    }
}
