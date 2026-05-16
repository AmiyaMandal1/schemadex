//! Postgres backend via sqlx. Reads from `information_schema` + `pg_catalog`
//! so it works on any Postgres ≥ 12.

use crate::error::Result;
use crate::introspector::{Backend, SchemaIntrospector};
use crate::model::{Column, DataType, ForeignKey, PrimaryKey};
use crate::sampling::{categorical_sample, numeric_sample, SamplingPolicy};
use async_trait::async_trait;
use sqlx::postgres::{PgPool, PgPoolOptions};
use sqlx::Row;

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

    async fn sample_column(
        &self,
        schema: &str,
        table: &str,
        col: &Column,
        policy: &SamplingPolicy,
    ) -> Result<Option<crate::model::ColumnSample>> {
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
            });
        }

        if let Some(policy) = self.sampling {
            for col in cols.iter_mut() {
                let sample = self
                    .sample_column(&schema, table, col, &policy)
                    .await
                    .ok()
                    .flatten();
                col.sample = sample;
            }
        }
        Ok(cols)
    }

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
