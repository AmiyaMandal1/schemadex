//! SQLite backend via sqlx. SQLite has no schemas (well, `main` and attached
//! databases — we ignore attachments by default).

use crate::error::Result;
use crate::introspector::{Backend, SchemaIntrospector};
use crate::model::{Column, DataType, ForeignKey, PrimaryKey};
use async_trait::async_trait;
use sqlx::sqlite::{SqlitePool, SqlitePoolOptions};
use sqlx::Row;

pub struct SqliteIntrospector {
    pool: SqlitePool,
}

impl SqliteIntrospector {
    pub async fn connect(url: &str) -> Result<Self> {
        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect(url)
            .await?;
        Ok(Self { pool })
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
        Ok(cols)
    }

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
