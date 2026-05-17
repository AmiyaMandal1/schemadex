//! Postgres backend via sqlx. Reads from `information_schema` + `pg_catalog`
//! so it works on any Postgres ≥ 12.

use crate::error::Result;
use crate::introspector::{
    estimate_tokens_from_bytes, Backend, QueryResult, QueryRunner, SchemaIntrospector,
};
use crate::model::{Column, DataType, ForeignKey, PrimaryKey};
use crate::sampling::{categorical_sample, numeric_sample, SamplingPolicy};
use async_trait::async_trait;
use futures::StreamExt;
use sqlx::postgres::{PgPool, PgPoolOptions};
use sqlx::{Column as _, Row, TypeInfo, ValueRef};

pub struct PostgresIntrospector {
    pool: PgPool,
    default_schema: String,
    pub sampling: Option<SamplingPolicy>,
}

impl PostgresIntrospector {
    pub async fn connect(url: &str) -> Result<Self> {
        let pool = PgPoolOptions::new().max_connections(8).connect(url).await?;
        Ok(Self {
            pool,
            default_schema: "public".to_string(),
            sampling: None,
        })
    }

    pub fn with_sampling(mut self, policy: SamplingPolicy) -> Self {
        self.sampling = Some(policy);
        self
    }

    fn schema_or_default<'a>(&'a self, s: Option<&'a str>) -> &'a str {
        s.unwrap_or(self.default_schema.as_str())
    }

    /// Populate `is_unique`, `check_constraint`, and `generation_expression`
    /// on the supplied columns. Each lookup is independent — a single
    /// failing query (e.g. lacking `pg_get_expr` privilege) only loses that
    /// dimension; the others still populate.
    async fn enrich_constraints(
        &self,
        schema: &str,
        table: &str,
        cols: &mut [Column],
    ) -> Result<()> {
        // 1) UNIQUE — either UNIQUE constraints or single-column UNIQUE indexes.
        // We treat *any* unique key that touches a column (alone) as making
        // that column unique. Composite unique keys do NOT mark their members
        // unique on their own.
        let uniq_sql = "SELECT a.attname AS column_name \
                        FROM pg_constraint c \
                        JOIN pg_class t ON t.oid = c.conrelid \
                        JOIN pg_namespace n ON n.oid = t.relnamespace \
                        JOIN unnest(c.conkey) WITH ORDINALITY AS k(attnum, ord) ON TRUE \
                        JOIN pg_attribute a ON a.attrelid = t.oid AND a.attnum = k.attnum \
                        WHERE c.contype = 'u' AND n.nspname = $1 AND t.relname = $2 \
                          AND array_length(c.conkey, 1) = 1";
        if let Ok(rows) = sqlx::query(uniq_sql)
            .bind(schema)
            .bind(table)
            .fetch_all(&self.pool)
            .await
        {
            for r in rows {
                if let Ok(name) = r.try_get::<String, _>("column_name") {
                    if let Some(c) = cols.iter_mut().find(|c| c.name == name) {
                        c.is_unique = true;
                    }
                }
            }
        }
        // Single-column UNIQUE indexes (covers UNIQUE INDEX as well as
        // implicit-unique-on-PK columns, which we still mark; PKs are unique).
        let uniq_idx_sql = "SELECT a.attname AS column_name \
                            FROM pg_index ix \
                            JOIN pg_class t ON t.oid = ix.indrelid \
                            JOIN pg_namespace n ON n.oid = t.relnamespace \
                            JOIN pg_attribute a ON a.attrelid = t.oid \
                             AND a.attnum = ANY(ix.indkey) \
                            WHERE ix.indisunique = true \
                              AND n.nspname = $1 AND t.relname = $2 \
                              AND array_length(ix.indkey, 1) = 1";
        if let Ok(rows) = sqlx::query(uniq_idx_sql)
            .bind(schema)
            .bind(table)
            .fetch_all(&self.pool)
            .await
        {
            for r in rows {
                if let Ok(name) = r.try_get::<String, _>("column_name") {
                    if let Some(c) = cols.iter_mut().find(|c| c.name == name) {
                        c.is_unique = true;
                    }
                }
            }
        }

        // 2) CHECK constraints — only attach those that reference exactly
        // one column. Multi-column checks would be misleading on a per-column
        // line.
        let check_sql = "SELECT a.attname AS column_name, \
                                pg_get_expr(c.conbin, c.conrelid) AS expr \
                         FROM pg_constraint c \
                         JOIN pg_class t ON t.oid = c.conrelid \
                         JOIN pg_namespace n ON n.oid = t.relnamespace \
                         JOIN unnest(c.conkey) AS k(attnum) ON TRUE \
                         JOIN pg_attribute a ON a.attrelid = t.oid AND a.attnum = k.attnum \
                         WHERE c.contype = 'c' AND n.nspname = $1 AND t.relname = $2 \
                           AND array_length(c.conkey, 1) = 1";
        if let Ok(rows) = sqlx::query(check_sql)
            .bind(schema)
            .bind(table)
            .fetch_all(&self.pool)
            .await
        {
            for r in rows {
                let name: Option<String> = r.try_get("column_name").ok();
                let expr: Option<String> = r.try_get("expr").ok();
                if let (Some(name), Some(expr)) = (name, expr) {
                    if let Some(c) = cols.iter_mut().find(|c| c.name == name) {
                        c.check_constraint = Some(expr);
                    }
                }
            }
        }

        // 3) Generated columns. `is_generated = 'ALWAYS'` lives in
        // information_schema.columns alongside the expression.
        let gen_sql = "SELECT column_name, generation_expression \
                       FROM information_schema.columns \
                       WHERE table_schema = $1 AND table_name = $2 \
                         AND is_generated = 'ALWAYS'";
        if let Ok(rows) = sqlx::query(gen_sql)
            .bind(schema)
            .bind(table)
            .fetch_all(&self.pool)
            .await
        {
            for r in rows {
                let name: Option<String> = r.try_get("column_name").ok();
                let expr: Option<String> = r.try_get("generation_expression").ok();
                if let (Some(name), Some(expr)) = (name, expr) {
                    if !expr.is_empty() {
                        if let Some(c) = cols.iter_mut().find(|c| c.name == name) {
                            c.generation_expression = Some(expr);
                        }
                    }
                }
            }
        }
        Ok(())
    }

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
            tracing::debug!(column = %col.name, "postgres.sample.redacted");
            return Ok(None);
        }
        if col.data_type.is_numeric() {
            let sql = format!(
                "SELECT \"{col}\"::double precision AS v FROM \"{schema}\".\"{table}\" \
                 WHERE \"{col}\" IS NOT NULL LIMIT {limit}",
                col = col.name,
                schema = schema,
                table = table,
                limit = policy.sample_rows,
            );
            let rows = sqlx::query(&sql).fetch_all(&self.pool).await?;
            let mut values: Vec<f64> = rows
                .iter()
                .map(|r| r.try_get::<f64, _>("v").unwrap_or(f64::NAN))
                .filter(|v| !v.is_nan())
                .collect();
            let null_sql = format!(
                "SELECT count(*) AS n FROM \"{schema}\".\"{table}\" WHERE \"{c}\" IS NULL",
                c = col.name,
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
                "SELECT \"{col}\"::text AS v, count(*)::bigint AS c \
                 FROM \"{schema}\".\"{table}\" \
                 WHERE \"{col}\" IS NOT NULL \
                 GROUP BY 1 ORDER BY c DESC LIMIT {topk}",
                col = col.name,
                schema = schema,
                table = table,
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
                "SELECT count(*) AS n FROM \"{schema}\".\"{table}\" WHERE \"{c}\" IS NULL",
                c = col.name,
            );
            let null_count: i64 = sqlx::query(&null_sql)
                .fetch_one(&self.pool)
                .await
                .ok()
                .and_then(|r| r.try_get::<i64, _>("n").ok())
                .unwrap_or(0);
            let distinct_sql = format!(
                "SELECT count(DISTINCT \"{c}\")::bigint AS n FROM \"{schema}\".\"{table}\"",
                c = col.name,
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

fn classify(pg_type: &str) -> DataType {
    let t = pg_type.to_lowercase();
    if t.starts_with("int") || t == "smallint" || t == "bigint" || t == "serial" || t == "bigserial"
    {
        DataType::Integer
    } else if t.starts_with("float") || t == "double precision" || t == "real" {
        DataType::Float
    } else if t.starts_with("numeric") || t.starts_with("decimal") {
        DataType::Decimal
    } else if t.starts_with("text")
        || t.starts_with("varchar")
        || t.starts_with("char")
        || t == "name"
    {
        DataType::Text
    } else if t == "boolean" || t == "bool" {
        DataType::Bool
    } else if t == "date" {
        DataType::Date
    } else if t.starts_with("time") {
        if t.contains("stamp") {
            DataType::Timestamp
        } else {
            DataType::Time
        }
    } else if t == "json" || t == "jsonb" {
        DataType::Json
    } else if t == "uuid" {
        DataType::Uuid
    } else if t == "bytea" {
        DataType::Bytes
    } else if t.ends_with("[]") {
        DataType::Array
    } else {
        DataType::Unknown
    }
}

#[async_trait]
impl SchemaIntrospector for PostgresIntrospector {
    fn backend(&self) -> Backend {
        Backend::Postgres
    }

    #[tracing::instrument(level = "debug", name = "postgres.tables", skip(self))]
    async fn tables(&self) -> Result<Vec<(Option<String>, String)>> {
        let rows = sqlx::query(
            "SELECT table_schema, table_name FROM information_schema.tables \
             WHERE table_type = 'BASE TABLE' \
               AND table_schema NOT IN ('pg_catalog','information_schema') \
             ORDER BY table_schema, table_name",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| {
                let s: String = r.try_get("table_schema").unwrap_or_default();
                let n: String = r.try_get("table_name").unwrap_or_default();
                (Some(s), n)
            })
            .collect())
    }

    #[tracing::instrument(level = "debug", name = "postgres.columns", skip(self))]
    async fn columns(&self, schema: Option<&str>, table: &str) -> Result<Vec<Column>> {
        let schema = self.schema_or_default(schema).to_string();
        let rows = sqlx::query(
            "SELECT column_name, data_type, is_nullable, column_default, ordinal_position \
             FROM information_schema.columns \
             WHERE table_schema = $1 AND table_name = $2 \
             ORDER BY ordinal_position",
        )
        .bind(&schema)
        .bind(table)
        .fetch_all(&self.pool)
        .await?;

        let mut cols = Vec::with_capacity(rows.len());
        for r in rows {
            let name: String = r.try_get("column_name")?;
            let native: String = r.try_get("data_type")?;
            let nullable: String = r.try_get("is_nullable")?;
            let default: Option<String> = r.try_get("column_default").ok();
            let ordinal: i32 = r.try_get::<i32, _>("ordinal_position").unwrap_or(0);
            cols.push(Column {
                name,
                data_type: classify(&native),
                native_type: native,
                nullable: nullable.eq_ignore_ascii_case("yes"),
                default,
                comment: None,
                ordinal,
                sample: None,
                check_constraint: None,
                is_unique: false,
                generation_expression: None,
            });
        }

        // Enrich with constraint-aware metadata: UNIQUE flags, CHECK
        // expressions, and generation expressions. Each query is best-effort;
        // a failure on the metadata side should not break the columns() call.
        if let Err(e) = self.enrich_constraints(&schema, table, &mut cols).await {
            tracing::debug!(error = %e, schema = %schema, table = %table, "postgres.columns.enrich_failed");
        }

        if let Some(policy) = self.sampling.as_ref() {
            for col in cols.iter_mut() {
                let sample = self
                    .sample_column(&schema, table, col, policy)
                    .await
                    .ok()
                    .flatten();
                col.sample = sample;
            }
        }
        Ok(cols)
    }

    #[tracing::instrument(level = "debug", name = "postgres.primary_key", skip(self))]
    async fn primary_key(&self, schema: Option<&str>, table: &str) -> Result<Option<PrimaryKey>> {
        let schema = self.schema_or_default(schema);
        let rows = sqlx::query(
            "SELECT kcu.constraint_name, kcu.column_name \
             FROM information_schema.table_constraints tc \
             JOIN information_schema.key_column_usage kcu \
               ON tc.constraint_name = kcu.constraint_name \
              AND tc.table_schema = kcu.table_schema \
             WHERE tc.constraint_type = 'PRIMARY KEY' \
               AND tc.table_schema = $1 AND tc.table_name = $2 \
             ORDER BY kcu.ordinal_position",
        )
        .bind(schema)
        .bind(table)
        .fetch_all(&self.pool)
        .await?;
        if rows.is_empty() {
            return Ok(None);
        }
        let name: Option<String> = rows.first().and_then(|r| r.try_get("constraint_name").ok());
        let columns: Vec<String> = rows
            .iter()
            .filter_map(|r| r.try_get("column_name").ok())
            .collect();
        Ok(Some(PrimaryKey { name, columns }))
    }

    #[tracing::instrument(level = "debug", name = "postgres.foreign_keys", skip(self))]
    async fn foreign_keys(&self, schema: Option<&str>, table: &str) -> Result<Vec<ForeignKey>> {
        let schema = self.schema_or_default(schema);
        let rows = sqlx::query(
            "SELECT tc.constraint_name, kcu.column_name, \
                    ccu.table_name AS referenced_table, ccu.column_name AS referenced_column \
             FROM information_schema.table_constraints tc \
             JOIN information_schema.key_column_usage kcu \
               ON tc.constraint_name = kcu.constraint_name \
              AND tc.table_schema = kcu.table_schema \
             JOIN information_schema.constraint_column_usage ccu \
               ON ccu.constraint_name = tc.constraint_name \
              AND ccu.table_schema = tc.table_schema \
             WHERE tc.constraint_type = 'FOREIGN KEY' \
               AND tc.table_schema = $1 AND tc.table_name = $2 \
             ORDER BY tc.constraint_name, kcu.ordinal_position",
        )
        .bind(schema)
        .bind(table)
        .fetch_all(&self.pool)
        .await?;

        use std::collections::BTreeMap;
        let mut by_name: BTreeMap<String, ForeignKey> = BTreeMap::new();
        for r in rows {
            let name: String = r.try_get("constraint_name")?;
            let col: String = r.try_get("column_name")?;
            let ref_tbl: String = r.try_get("referenced_table")?;
            let ref_col: String = r.try_get("referenced_column")?;
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
        let schema = self.schema_or_default(schema);
        let row = sqlx::query(
            "SELECT reltuples::bigint AS n FROM pg_class c \
             JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = $1 AND c.relname = $2",
        )
        .bind(schema)
        .bind(table)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row
            .and_then(|r| r.try_get::<i64, _>("n").ok())
            .map(|n| n.max(0) as u64))
    }

    async fn table_comment(&self, schema: Option<&str>, table: &str) -> Result<Option<String>> {
        let schema = self.schema_or_default(schema);
        let row = sqlx::query(
            "SELECT obj_description(c.oid) AS d FROM pg_class c \
             JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = $1 AND c.relname = $2",
        )
        .bind(schema)
        .bind(table)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row
            .and_then(|r| r.try_get::<Option<String>, _>("d").ok())
            .flatten())
    }
}

/// Render a postgres cell as a string. Tries text-first then falls back to
/// typed reads for the common scalar types. Anything we can't classify becomes
/// `<unsupported: <type>>` rather than panicking — agents handle that better
/// than a 500.
fn pg_cell_to_string(row: &sqlx::postgres::PgRow, idx: usize) -> String {
    let raw = match row.try_get_raw(idx) {
        Ok(r) => r,
        Err(_) => return String::new(),
    };
    if raw.is_null() {
        return String::new();
    }
    let type_name = raw.type_info().name().to_string();

    // Fast path: most builtins decode straight to a string via the TEXT codec.
    if let Ok(s) = row.try_get::<String, _>(idx) {
        return s;
    }

    match type_name.as_str() {
        "INT2" | "INT4" => row
            .try_get::<i32, _>(idx)
            .map(|v| v.to_string())
            .unwrap_or_default(),
        "INT8" => row
            .try_get::<i64, _>(idx)
            .map(|v| v.to_string())
            .unwrap_or_default(),
        "FLOAT4" => row
            .try_get::<f32, _>(idx)
            .map(|v| v.to_string())
            .unwrap_or_default(),
        "FLOAT8" => row
            .try_get::<f64, _>(idx)
            .map(|v| v.to_string())
            .unwrap_or_default(),
        "BOOL" => row
            .try_get::<bool, _>(idx)
            .map(|v| v.to_string())
            .unwrap_or_default(),
        other => format!("<unsupported: {other}>"),
    }
}

#[async_trait]
impl QueryRunner for PostgresIntrospector {
    #[tracing::instrument(
        level = "debug",
        name = "postgres.run_sql",
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
                row.push(pg_cell_to_string(r, i));
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
        name = "postgres.run_sql_streaming",
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
                let cell = pg_cell_to_string(&row, i);
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

    #[tracing::instrument(
        level = "debug",
        name = "postgres.preview_cost",
        skip(self, sql),
        fields(sql_len = sql.len()),
    )]
    async fn preview_cost(&self, sql: &str) -> Result<crate::CostEstimate> {
        // EXPLAIN (FORMAT JSON) returns a single JSON column with a
        // one-element array; `Plan.Plan Rows` and `Plan.Total Cost` live
        // under the top-level `Plan` object.
        let explain_sql = format!("EXPLAIN (FORMAT JSON) {sql}");
        let row = match sqlx::query(&explain_sql).fetch_one(&self.pool).await {
            Ok(r) => r,
            Err(e) => {
                return Ok(crate::CostEstimate {
                    bytes_processed: None,
                    rows_estimate: None,
                    warning: Some(format!("EXPLAIN failed: {e}")),
                });
            }
        };
        // First column may come back as a `serde_json::Value` directly,
        // or as a string depending on driver coercion. Try Value first.
        let val: serde_json::Value = match row.try_get::<serde_json::Value, _>(0) {
            Ok(v) => v,
            Err(_) => match row.try_get::<String, _>(0) {
                Ok(s) => serde_json::from_str(&s).unwrap_or(serde_json::Value::Null),
                Err(e) => {
                    return Ok(crate::CostEstimate {
                        bytes_processed: None,
                        rows_estimate: None,
                        warning: Some(format!("decode EXPLAIN row: {e}")),
                    });
                }
            },
        };
        // Drill into `[0]["Plan"]`.
        let plan = val
            .as_array()
            .and_then(|arr| arr.first())
            .and_then(|item| item.get("Plan"));
        let rows_estimate = plan
            .and_then(|p| p.get("Plan Rows"))
            .and_then(|v| v.as_u64());
        let total_cost = plan
            .and_then(|p| p.get("Total Cost"))
            .and_then(|v| v.as_f64());
        let warning = if rows_estimate.is_none() && total_cost.is_none() {
            Some("EXPLAIN returned no Plan section".to_string())
        } else {
            total_cost.map(|c| format!("postgres total_cost={c:.2}"))
        };
        Ok(crate::CostEstimate {
            bytes_processed: None,
            rows_estimate,
            warning,
        })
    }
}
