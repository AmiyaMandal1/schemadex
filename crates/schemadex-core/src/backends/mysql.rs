//! MySQL backend via sqlx. Reads from `information_schema` so it works on
//! any MySQL >= 5.7 / MariaDB >= 10.x. No `pg_catalog`-style fallback path —
//! MySQL exposes everything we need through the standard views.

use crate::error::Result;
use crate::introspector::{
    estimate_tokens_from_bytes, Backend, QueryResult, QueryRunner, SchemaIntrospector,
};
use crate::model::{Column, DataType, ForeignKey, PrimaryKey};
use crate::sampling::{categorical_sample, numeric_sample, SamplingPolicy};
use async_trait::async_trait;
use futures::StreamExt;
use sqlx::mysql::{MySqlPool, MySqlPoolOptions};
use sqlx::{Column as _, Row, TypeInfo, ValueRef};

pub struct MysqlIntrospector {
    pool: MySqlPool,
    pub sampling: Option<SamplingPolicy>,
}

impl MysqlIntrospector {
    pub async fn connect(url: &str) -> Result<Self> {
        let pool = MySqlPoolOptions::new()
            .max_connections(8)
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
        schema: Option<&str>,
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
            tracing::debug!(column = %col.name, "mysql.sample.redacted");
            return Ok(None);
        }
        let t = table.replace('`', "");
        let c = col.name.replace('`', "");
        let qualified = match schema {
            Some(s) => {
                let s = s.replace('`', "");
                format!("`{s}`.`{t}`")
            }
            None => format!("`{t}`"),
        };
        if col.data_type.is_numeric() {
            let sql = format!(
                "SELECT `{c}` AS v FROM {qualified} \
                 WHERE `{c}` IS NOT NULL LIMIT {limit}",
                c = c,
                qualified = qualified,
                limit = policy.sample_rows,
            );
            let rows = sqlx::query(&sql).fetch_all(&self.pool).await?;
            // MySQL numeric columns can be SIGNED, UNSIGNED, or DECIMAL; try
            // f64 first, then fall back through u64/i64 like the cell renderer.
            let mut values: Vec<f64> = rows
                .iter()
                .map(|r| {
                    r.try_get::<f64, _>("v")
                        .ok()
                        .or_else(|| r.try_get::<u64, _>("v").ok().map(|v| v as f64))
                        .or_else(|| r.try_get::<i64, _>("v").ok().map(|v| v as f64))
                        .unwrap_or(f64::NAN)
                })
                .filter(|v| !v.is_nan())
                .collect();
            let null_sql = format!(
                "SELECT count(*) AS n FROM {qualified} WHERE `{c}` IS NULL",
                qualified = qualified,
                c = c,
            );
            let null_count: i64 = sqlx::query(&null_sql)
                .fetch_one(&self.pool)
                .await
                .ok()
                .and_then(|r| {
                    r.try_get::<i64, _>("n")
                        .ok()
                        .or_else(|| r.try_get::<u64, _>("n").ok().map(|v| v as i64))
                })
                .unwrap_or(0);
            Ok(Some(numeric_sample(&mut values, null_count.max(0) as u64)))
        } else if col.data_type.is_categorical() {
            let sql = format!(
                "SELECT CAST(`{c}` AS CHAR) AS v, count(*) AS c \
                 FROM {qualified} \
                 WHERE `{c}` IS NOT NULL \
                 GROUP BY 1 ORDER BY count(*) DESC LIMIT {topk}",
                c = c,
                qualified = qualified,
                topk = policy.top_k,
            );
            let rows = sqlx::query(&sql).fetch_all(&self.pool).await?;
            let top: Vec<(String, u64)> = rows
                .iter()
                .filter_map(|r| {
                    let v: Option<String> = r.try_get("v").ok();
                    let c: Option<u64> = r
                        .try_get::<i64, _>("c")
                        .ok()
                        .map(|v| v.max(0) as u64)
                        .or_else(|| r.try_get::<u64, _>("c").ok());
                    Some((v?, c?))
                })
                .collect();
            let total_non_null: u64 = top.iter().map(|(_, c)| *c).sum();
            let null_sql = format!(
                "SELECT count(*) AS n FROM {qualified} WHERE `{c}` IS NULL",
                qualified = qualified,
                c = c,
            );
            let null_count: u64 = sqlx::query(&null_sql)
                .fetch_one(&self.pool)
                .await
                .ok()
                .and_then(|r| {
                    r.try_get::<i64, _>("n")
                        .ok()
                        .map(|v| v.max(0) as u64)
                        .or_else(|| r.try_get::<u64, _>("n").ok())
                })
                .unwrap_or(0);
            let distinct_sql = format!(
                "SELECT count(DISTINCT `{c}`) AS n FROM {qualified}",
                c = c,
                qualified = qualified,
            );
            let distinct: Option<u64> = sqlx::query(&distinct_sql)
                .fetch_one(&self.pool)
                .await
                .ok()
                .and_then(|r| {
                    r.try_get::<i64, _>("n")
                        .ok()
                        .map(|v| v.max(0) as u64)
                        .or_else(|| r.try_get::<u64, _>("n").ok())
                });
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

/// Map a MySQL `data_type` (the value returned by `information_schema.columns`)
/// to the coarse [`DataType`] enum. Matches the shape of `postgres::classify`
/// and `sqlite::classify`.
pub fn classify_mysql(native: &str) -> DataType {
    let t = native.to_lowercase();
    // Integer family: tinyint, smallint, mediumint, int, integer, bigint, year.
    if t == "tinyint"
        || t == "smallint"
        || t == "mediumint"
        || t == "int"
        || t == "integer"
        || t == "bigint"
        || t == "year"
    {
        DataType::Integer
    } else if t == "float" || t == "double" || t == "real" {
        DataType::Float
    } else if t == "decimal" || t == "numeric" || t == "dec" || t == "fixed" {
        DataType::Decimal
    } else if t == "char"
        || t == "varchar"
        || t == "tinytext"
        || t == "text"
        || t == "mediumtext"
        || t == "longtext"
        || t == "enum"
        || t == "set"
    {
        DataType::Text
    } else if t == "bool" || t == "boolean" {
        DataType::Bool
    } else if t == "date" {
        DataType::Date
    } else if t == "time" {
        DataType::Time
    } else if t == "datetime" || t == "timestamp" {
        DataType::Timestamp
    } else if t == "json" {
        DataType::Json
    } else if t == "binary"
        || t == "varbinary"
        || t == "tinyblob"
        || t == "blob"
        || t == "mediumblob"
        || t == "longblob"
        || t == "bit"
    {
        DataType::Bytes
    } else {
        DataType::Unknown
    }
}

#[async_trait]
impl SchemaIntrospector for MysqlIntrospector {
    fn backend(&self) -> Backend {
        Backend::Mysql
    }

    #[tracing::instrument(level = "debug", name = "mysql.tables", skip(self))]
    async fn tables(&self) -> Result<Vec<(Option<String>, String)>> {
        let rows = sqlx::query(
            "SELECT table_schema, table_name FROM information_schema.tables \
             WHERE table_schema = DATABASE() AND table_type = 'BASE TABLE' \
             ORDER BY table_schema, table_name",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| {
                let s: String = r.try_get("TABLE_SCHEMA").or_else(|_| r.try_get("table_schema")).unwrap_or_default();
                let n: String = r.try_get("TABLE_NAME").or_else(|_| r.try_get("table_name")).unwrap_or_default();
                (Some(s), n)
            })
            .collect())
    }

    #[tracing::instrument(level = "debug", name = "mysql.columns", skip(self))]
    async fn columns(&self, schema: Option<&str>, table: &str) -> Result<Vec<Column>> {
        // information_schema.columns: filter by the requested schema if given,
        // otherwise by DATABASE() (the connection's default).
        let rows = match schema {
            Some(s) => sqlx::query(
                "SELECT column_name, data_type, is_nullable, column_default, ordinal_position, column_comment \
                 FROM information_schema.columns \
                 WHERE table_schema = ? AND table_name = ? \
                 ORDER BY ordinal_position",
            )
            .bind(s)
            .bind(table)
            .fetch_all(&self.pool)
            .await?,
            None => sqlx::query(
                "SELECT column_name, data_type, is_nullable, column_default, ordinal_position, column_comment \
                 FROM information_schema.columns \
                 WHERE table_schema = DATABASE() AND table_name = ? \
                 ORDER BY ordinal_position",
            )
            .bind(table)
            .fetch_all(&self.pool)
            .await?,
        };

        let mut cols = Vec::with_capacity(rows.len());
        for r in rows {
            // information_schema columns are uppercase in MySQL 8 strict mode
            // but the driver normalizes via try_get on lowercase. Try both.
            let name: String = r
                .try_get("COLUMN_NAME")
                .or_else(|_| r.try_get("column_name"))?;
            let native: String = r
                .try_get("DATA_TYPE")
                .or_else(|_| r.try_get("data_type"))?;
            let nullable: String = r
                .try_get("IS_NULLABLE")
                .or_else(|_| r.try_get("is_nullable"))?;
            let default: Option<String> = r
                .try_get("COLUMN_DEFAULT")
                .or_else(|_| r.try_get("column_default"))
                .ok();
            // ORDINAL_POSITION comes back as u64 from MySQL; cast down.
            let ordinal: i64 = r
                .try_get::<i64, _>("ORDINAL_POSITION")
                .or_else(|_| r.try_get::<i64, _>("ordinal_position"))
                .unwrap_or_else(|_| {
                    r.try_get::<u64, _>("ORDINAL_POSITION")
                        .or_else(|_| r.try_get::<u64, _>("ordinal_position"))
                        .unwrap_or(0) as i64
                });
            let comment: Option<String> = r
                .try_get::<Option<String>, _>("COLUMN_COMMENT")
                .or_else(|_| r.try_get::<Option<String>, _>("column_comment"))
                .ok()
                .flatten()
                .filter(|s| !s.is_empty());
            cols.push(Column {
                name,
                data_type: classify_mysql(&native),
                native_type: native,
                nullable: nullable.eq_ignore_ascii_case("yes"),
                default,
                comment,
                ordinal: ordinal as i32,
                sample: None,
            });
        }

        if let Some(policy) = self.sampling.as_ref() {
            for col in cols.iter_mut() {
                let sample = self
                    .sample_column(schema, table, col, policy)
                    .await
                    .ok()
                    .flatten();
                col.sample = sample;
            }
        }
        Ok(cols)
    }

    #[tracing::instrument(level = "debug", name = "mysql.primary_key", skip(self))]
    async fn primary_key(&self, schema: Option<&str>, table: &str) -> Result<Option<PrimaryKey>> {
        let rows = match schema {
            Some(s) => sqlx::query(
                "SELECT kcu.constraint_name, kcu.column_name \
                 FROM information_schema.table_constraints tc \
                 JOIN information_schema.key_column_usage kcu \
                   ON tc.constraint_name = kcu.constraint_name \
                  AND tc.table_schema = kcu.table_schema \
                  AND tc.table_name = kcu.table_name \
                 WHERE tc.constraint_type = 'PRIMARY KEY' \
                   AND tc.table_schema = ? AND tc.table_name = ? \
                 ORDER BY kcu.ordinal_position",
            )
            .bind(s)
            .bind(table)
            .fetch_all(&self.pool)
            .await?,
            None => sqlx::query(
                "SELECT kcu.constraint_name, kcu.column_name \
                 FROM information_schema.table_constraints tc \
                 JOIN information_schema.key_column_usage kcu \
                   ON tc.constraint_name = kcu.constraint_name \
                  AND tc.table_schema = kcu.table_schema \
                  AND tc.table_name = kcu.table_name \
                 WHERE tc.constraint_type = 'PRIMARY KEY' \
                   AND tc.table_schema = DATABASE() AND tc.table_name = ? \
                 ORDER BY kcu.ordinal_position",
            )
            .bind(table)
            .fetch_all(&self.pool)
            .await?,
        };
        if rows.is_empty() {
            return Ok(None);
        }
        let name: Option<String> = rows.first().and_then(|r| {
            r.try_get("CONSTRAINT_NAME")
                .or_else(|_| r.try_get("constraint_name"))
                .ok()
        });
        let columns: Vec<String> = rows
            .iter()
            .filter_map(|r| {
                r.try_get("COLUMN_NAME")
                    .or_else(|_| r.try_get("column_name"))
                    .ok()
            })
            .collect();
        Ok(Some(PrimaryKey { name, columns }))
    }

    #[tracing::instrument(level = "debug", name = "mysql.foreign_keys", skip(self))]
    async fn foreign_keys(&self, schema: Option<&str>, table: &str) -> Result<Vec<ForeignKey>> {
        let rows = match schema {
            Some(s) => sqlx::query(
                "SELECT constraint_name, column_name, referenced_table_name, referenced_column_name \
                 FROM information_schema.key_column_usage \
                 WHERE table_schema = ? AND table_name = ? AND referenced_table_name IS NOT NULL \
                 ORDER BY constraint_name, ordinal_position",
            )
            .bind(s)
            .bind(table)
            .fetch_all(&self.pool)
            .await?,
            None => sqlx::query(
                "SELECT constraint_name, column_name, referenced_table_name, referenced_column_name \
                 FROM information_schema.key_column_usage \
                 WHERE table_schema = DATABASE() AND table_name = ? AND referenced_table_name IS NOT NULL \
                 ORDER BY constraint_name, ordinal_position",
            )
            .bind(table)
            .fetch_all(&self.pool)
            .await?,
        };

        use std::collections::BTreeMap;
        let mut by_name: BTreeMap<String, ForeignKey> = BTreeMap::new();
        for r in rows {
            let name: String = r
                .try_get("CONSTRAINT_NAME")
                .or_else(|_| r.try_get("constraint_name"))?;
            let col: String = r
                .try_get("COLUMN_NAME")
                .or_else(|_| r.try_get("column_name"))?;
            let ref_tbl: String = r
                .try_get("REFERENCED_TABLE_NAME")
                .or_else(|_| r.try_get("referenced_table_name"))?;
            let ref_col: String = r
                .try_get("REFERENCED_COLUMN_NAME")
                .or_else(|_| r.try_get("referenced_column_name"))?;
            let fk = by_name.entry(name.clone()).or_insert(ForeignKey {
                name: Some(name),
                columns: vec![],
                referenced_table: ref_tbl,
                referenced_columns: vec![],
            });
            fk.columns.push(col);
            fk.referenced_columns.push(ref_col);
        }
        Ok(by_name.into_values().collect())
    }

    async fn row_count_estimate(&self, schema: Option<&str>, table: &str) -> Result<Option<u64>> {
        // `table_rows` is an estimate for InnoDB and exact for MyISAM; either
        // way it's free and good enough for the agent prompt.
        let row = match schema {
            Some(s) => sqlx::query(
                "SELECT table_rows FROM information_schema.tables \
                 WHERE table_schema = ? AND table_name = ?",
            )
            .bind(s)
            .bind(table)
            .fetch_optional(&self.pool)
            .await?,
            None => sqlx::query(
                "SELECT table_rows FROM information_schema.tables \
                 WHERE table_schema = DATABASE() AND table_name = ?",
            )
            .bind(table)
            .fetch_optional(&self.pool)
            .await?,
        };
        Ok(row.and_then(|r| {
            r.try_get::<u64, _>("TABLE_ROWS")
                .or_else(|_| r.try_get::<u64, _>("table_rows"))
                .ok()
        }))
    }

    async fn table_comment(&self, schema: Option<&str>, table: &str) -> Result<Option<String>> {
        let row = match schema {
            Some(s) => sqlx::query(
                "SELECT table_comment FROM information_schema.tables \
                 WHERE table_schema = ? AND table_name = ?",
            )
            .bind(s)
            .bind(table)
            .fetch_optional(&self.pool)
            .await?,
            None => sqlx::query(
                "SELECT table_comment FROM information_schema.tables \
                 WHERE table_schema = DATABASE() AND table_name = ?",
            )
            .bind(table)
            .fetch_optional(&self.pool)
            .await?,
        };
        Ok(row
            .and_then(|r| {
                r.try_get::<Option<String>, _>("TABLE_COMMENT")
                    .or_else(|_| r.try_get::<Option<String>, _>("table_comment"))
                    .ok()
            })
            .flatten()
            .filter(|s| !s.is_empty()))
    }
}

/// Render a MySQL cell as a string. NULL becomes the empty string so the agent
/// markdown stays clean; unknown types fall back to `<unsupported: <type>>`
/// instead of panicking.
fn mysql_cell_to_string(row: &sqlx::mysql::MySqlRow, idx: usize) -> String {
    let raw = match row.try_get_raw(idx) {
        Ok(r) => r,
        Err(_) => return String::new(),
    };
    if raw.is_null() {
        return String::new();
    }
    let type_name = raw.type_info().name().to_string();

    // Fast path: most string-like types decode directly.
    if let Ok(s) = row.try_get::<String, _>(idx) {
        return s;
    }

    match type_name.as_str() {
        "TINYINT" | "SMALLINT" | "MEDIUMINT" | "INT" | "YEAR" => row
            .try_get::<i32, _>(idx)
            .map(|v| v.to_string())
            .unwrap_or_default(),
        "BIGINT" => row
            .try_get::<i64, _>(idx)
            .map(|v| v.to_string())
            .unwrap_or_default(),
        "TINYINT UNSIGNED" | "SMALLINT UNSIGNED" | "MEDIUMINT UNSIGNED" | "INT UNSIGNED" => row
            .try_get::<u32, _>(idx)
            .map(|v| v.to_string())
            .unwrap_or_default(),
        "BIGINT UNSIGNED" => row
            .try_get::<u64, _>(idx)
            .map(|v| v.to_string())
            .unwrap_or_default(),
        "FLOAT" => row
            .try_get::<f32, _>(idx)
            .map(|v| v.to_string())
            .unwrap_or_default(),
        "DOUBLE" => row
            .try_get::<f64, _>(idx)
            .map(|v| v.to_string())
            .unwrap_or_default(),
        "BOOLEAN" => row
            .try_get::<bool, _>(idx)
            .map(|v| v.to_string())
            .unwrap_or_default(),
        "BLOB" | "TINYBLOB" | "MEDIUMBLOB" | "LONGBLOB" | "VARBINARY" | "BINARY" => row
            .try_get::<Vec<u8>, _>(idx)
            .map(|v| format!("<{} bytes>", v.len()))
            .unwrap_or_default(),
        other => format!("<unsupported: {other}>"),
    }
}

#[async_trait]
impl QueryRunner for MysqlIntrospector {
    #[tracing::instrument(
        level = "debug",
        name = "mysql.run_sql",
        skip(self, sql),
        fields(sql_len = sql.len(), row_limit),
    )]
    async fn run_sql(&self, sql: &str, row_limit: usize) -> Result<QueryResult> {
        let rows = sqlx::query(sql).fetch_all(&self.pool).await?;
        let truncated = rows.len() > row_limit;
        let take = rows.len().min(row_limit);

        let columns: Vec<String> = rows
            .first()
            .map(|r| r.columns().iter().map(|c| c.name().to_string()).collect())
            .unwrap_or_default();

        let mut out_rows = Vec::with_capacity(take);
        for r in rows.iter().take(take) {
            let mut row = Vec::with_capacity(columns.len());
            for i in 0..r.columns().len() {
                row.push(mysql_cell_to_string(r, i));
            }
            out_rows.push(row);
        }

        Ok(QueryResult {
            columns,
            rows: out_rows,
            truncated,
        })
    }

    #[tracing::instrument(
        level = "debug",
        name = "mysql.run_sql_streaming",
        skip(self, sql),
        fields(sql_len = sql.len(), token_budget),
    )]
    async fn run_sql_streaming(&self, sql: &str, token_budget: usize) -> Result<QueryResult> {
        let mut stream = sqlx::query(sql).fetch(&self.pool);
        let mut columns: Vec<String> = Vec::new();
        let mut out_rows: Vec<Vec<String>> = Vec::new();
        let mut tokens_so_far: usize = 0;
        let mut truncated = false;

        while let Some(row) = stream.next().await {
            let row = row?;
            if columns.is_empty() {
                columns = row.columns().iter().map(|c| c.name().to_string()).collect();
            }
            let mut cells = Vec::with_capacity(columns.len());
            let mut row_bytes: usize = 0;
            for i in 0..row.columns().len() {
                let cell = mysql_cell_to_string(&row, i);
                row_bytes = row_bytes.saturating_add(cell.len());
                cells.push(cell);
            }
            let row_tokens = estimate_tokens_from_bytes(row_bytes);
            if tokens_so_far.saturating_add(row_tokens) > token_budget && !out_rows.is_empty() {
                truncated = true;
                break;
            }
            tokens_so_far = tokens_so_far.saturating_add(row_tokens);
            out_rows.push(cells);
        }

        Ok(QueryResult {
            columns,
            rows: out_rows,
            truncated,
        })
    }
}
