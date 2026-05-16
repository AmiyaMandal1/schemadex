//! BigQuery backend via `gcp-bigquery-client`.
//!
//! Authentication picks one of three paths, in order:
//!   1. `GOOGLE_APPLICATION_CREDENTIALS` -> service-account key file
//!   2. Application Default Credentials (gcloud SDK)
//!   3. Error — we don't try to silently fall back to anonymous, since the
//!      BigQuery API will give a much worse error in that case.
//!
//! URL shape: `bigquery://project[/dataset]`. When a dataset is supplied,
//! `tables()` is scoped to that dataset; otherwise we walk every dataset in
//! the project (which is slow but matches what `connect("bigquery://proj")`
//! ought to do).
//!
//! BigQuery has no enforced foreign keys and no per-table primary-key
//! metadata in the public API surface — `primary_key()` and
//! `foreign_keys()` therefore return empty results. Schema and column info
//! come from `table().get(...)`.

use crate::error::{Result, SchemadexError};
use crate::introspector::{Backend, QueryResult, QueryRunner, SchemaIntrospector};
use crate::model::{Column, DataType, ForeignKey, PrimaryKey};
use async_trait::async_trait;
use gcp_bigquery_client::model::field_type::FieldType;
use gcp_bigquery_client::model::query_request::QueryRequest;
use gcp_bigquery_client::Client;

pub struct BigQueryIntrospector {
    client: Client,
    pub project: String,
    pub dataset: Option<String>,
}

impl BigQueryIntrospector {
    /// Build a client from a URL of the form `bigquery://project[/dataset]`.
    /// Auth is picked up from the environment (see module docs).
    pub async fn connect(url: &str) -> Result<Self> {
        let (project, dataset) = parse_bigquery_url(url)?;
        let client = build_client().await?;
        Ok(Self {
            client,
            project,
            dataset,
        })
    }
}

fn parse_bigquery_url(url: &str) -> Result<(String, Option<String>)> {
    let trimmed = url.trim_start_matches("bigquery://");
    let mut parts = trimmed.splitn(2, '/');
    let project = parts.next().unwrap_or("").to_string();
    let dataset = parts
        .next()
        .map(str::to_string)
        .filter(|s| !s.is_empty());
    if project.is_empty() {
        return Err(SchemadexError::Other(
            "BigQuery URL must include a project: bigquery://project[/dataset]".into(),
        ));
    }
    Ok((project, dataset))
}

async fn build_client() -> Result<Client> {
    if let Ok(path) = std::env::var("GOOGLE_APPLICATION_CREDENTIALS") {
        if !path.is_empty() {
            return Client::from_service_account_key_file(&path)
                .await
                .map_err(|e| {
                    SchemadexError::Other(format!(
                        "bigquery service-account auth failed: {e}"
                    ))
                });
        }
    }
    Client::from_application_default_credentials()
        .await
        .map_err(|e| {
            SchemadexError::Other(format!(
                "bigquery ADC auth failed (set GOOGLE_APPLICATION_CREDENTIALS or run \
                 `gcloud auth application-default login`): {e}"
            ))
        })
}

/// Map a BigQuery [`FieldType`] into the coarse [`DataType`] bucket. RECORD
/// and STRUCT collapse into `Unknown` — we don't model nested rows in the
/// agent surface yet.
pub fn bigquery_classify(field_type: &FieldType) -> DataType {
    match field_type {
        FieldType::String => DataType::Text,
        FieldType::Bytes => DataType::Bytes,
        FieldType::Integer | FieldType::Int64 => DataType::Integer,
        FieldType::Float | FieldType::Float64 => DataType::Float,
        FieldType::Numeric | FieldType::Bignumeric => DataType::Decimal,
        FieldType::Boolean | FieldType::Bool => DataType::Bool,
        FieldType::Timestamp | FieldType::Datetime => DataType::Timestamp,
        FieldType::Date => DataType::Date,
        FieldType::Time => DataType::Time,
        FieldType::Json => DataType::Json,
        FieldType::Geography => DataType::Text,
        FieldType::Interval => DataType::Text,
        FieldType::Record | FieldType::Struct => DataType::Unknown,
    }
}

fn field_native(field_type: &FieldType) -> String {
    match field_type {
        FieldType::String => "STRING",
        FieldType::Bytes => "BYTES",
        FieldType::Integer => "INTEGER",
        FieldType::Int64 => "INT64",
        FieldType::Float => "FLOAT",
        FieldType::Float64 => "FLOAT64",
        FieldType::Numeric => "NUMERIC",
        FieldType::Bignumeric => "BIGNUMERIC",
        FieldType::Boolean => "BOOLEAN",
        FieldType::Bool => "BOOL",
        FieldType::Timestamp => "TIMESTAMP",
        FieldType::Datetime => "DATETIME",
        FieldType::Date => "DATE",
        FieldType::Time => "TIME",
        FieldType::Json => "JSON",
        FieldType::Geography => "GEOGRAPHY",
        FieldType::Interval => "INTERVAL",
        FieldType::Record => "RECORD",
        FieldType::Struct => "STRUCT",
    }
    .to_string()
}

#[async_trait]
impl SchemaIntrospector for BigQueryIntrospector {
    fn backend(&self) -> Backend {
        Backend::BigQuery
    }

    #[tracing::instrument(level = "debug", name = "bigquery.tables", skip(self))]
    async fn tables(&self) -> Result<Vec<(Option<String>, String)>> {
        let mut out: Vec<(Option<String>, String)> = Vec::new();
        let dataset_ids: Vec<String> = match &self.dataset {
            Some(d) => vec![d.clone()],
            None => {
                let datasets = self
                    .client
                    .dataset()
                    .list(&self.project, Default::default())
                    .await
                    .map_err(|e| {
                        SchemadexError::Other(format!("bigquery dataset list: {e}"))
                    })?;
                datasets
                    .datasets
                    .into_iter()
                    .map(|d| d.dataset_reference.dataset_id)
                    .collect()
            }
        };

        for dataset_id in dataset_ids {
            let mut page_token: Option<String> = None;
            loop {
                let mut opts = gcp_bigquery_client::table::ListOptions::default()
                    .max_results(500);
                if let Some(t) = page_token.clone() {
                    opts = opts.page_token(t);
                }
                let list = self
                    .client
                    .table()
                    .list(&self.project, &dataset_id, opts)
                    .await
                    .map_err(|e| {
                        SchemadexError::Other(format!("bigquery table list: {e}"))
                    })?;
                if let Some(tables) = list.tables {
                    for t in tables {
                        out.push((
                            Some(dataset_id.clone()),
                            t.table_reference.table_id,
                        ));
                    }
                }
                page_token = list.next_page_token;
                if page_token.is_none() {
                    break;
                }
            }
        }
        Ok(out)
    }

    #[tracing::instrument(level = "debug", name = "bigquery.columns", skip(self))]
    async fn columns(&self, schema: Option<&str>, table: &str) -> Result<Vec<Column>> {
        let dataset = schema
            .map(str::to_string)
            .or_else(|| self.dataset.clone())
            .ok_or_else(|| {
                SchemadexError::Other(
                    "bigquery columns(): dataset must be supplied via URL or schema arg".into(),
                )
            })?;
        let table = self
            .client
            .table()
            .get(&self.project, &dataset, table, None)
            .await
            .map_err(|e| SchemadexError::Other(format!("bigquery table get: {e}")))?;
        let schema = match table.schema.fields {
            Some(fields) => fields,
            None => return Ok(vec![]),
        };
        let mut cols = Vec::with_capacity(schema.len());
        for (i, f) in schema.iter().enumerate() {
            let nullable = f
                .mode
                .as_deref()
                .map(|m| !m.eq_ignore_ascii_case("REQUIRED"))
                .unwrap_or(true);
            cols.push(Column {
                name: f.name.clone(),
                data_type: bigquery_classify(&f.r#type),
                native_type: field_native(&f.r#type),
                nullable,
                default: None,
                comment: f.description.clone(),
                ordinal: (i + 1) as i32,
                sample: None,
            });
        }
        Ok(cols)
    }

    /// BigQuery does not expose primary keys through the standard table API.
    /// We deliberately return `None` rather than failing, so callers can fan
    /// out introspection without special-casing the backend.
    #[tracing::instrument(level = "debug", name = "bigquery.primary_key", skip(self))]
    async fn primary_key(
        &self,
        _schema: Option<&str>,
        _table: &str,
    ) -> Result<Option<PrimaryKey>> {
        Ok(None)
    }

    /// BigQuery does not enforce foreign keys. Returning an empty list keeps
    /// downstream rendering and fingerprinting backend-agnostic.
    #[tracing::instrument(level = "debug", name = "bigquery.foreign_keys", skip(self))]
    async fn foreign_keys(
        &self,
        _schema: Option<&str>,
        _table: &str,
    ) -> Result<Vec<ForeignKey>> {
        Ok(vec![])
    }

    async fn table_comment(
        &self,
        schema: Option<&str>,
        table: &str,
    ) -> Result<Option<String>> {
        let dataset = match schema.map(str::to_string).or_else(|| self.dataset.clone()) {
            Some(d) => d,
            None => return Ok(None),
        };
        let t = self
            .client
            .table()
            .get(&self.project, &dataset, table, None)
            .await
            .map_err(|e| SchemadexError::Other(format!("bigquery table get: {e}")))?;
        Ok(t.description.filter(|s| !s.is_empty()))
    }
}

#[async_trait]
impl QueryRunner for BigQueryIntrospector {
    #[tracing::instrument(
        level = "debug",
        name = "bigquery.run_sql",
        skip(self, sql),
        fields(sql_len = sql.len(), row_limit),
    )]
    async fn run_sql(&self, sql: &str, row_limit: usize) -> Result<QueryResult> {
        // Fetch one extra row so we can flag truncation without an extra round-trip.
        let mut req = QueryRequest::new(sql.to_string());
        let cap = (row_limit + 1).min(i32::MAX as usize) as i32;
        req.max_results = Some(cap);
        let resp = self
            .client
            .job()
            .query(&self.project, req)
            .await
            .map_err(|e| SchemadexError::Other(format!("bigquery query: {e}")))?;

        let columns: Vec<String> = resp
            .schema
            .as_ref()
            .and_then(|s| s.fields.as_ref())
            .map(|fields| fields.iter().map(|f| f.name.clone()).collect())
            .unwrap_or_default();

        let raw_rows = resp.rows.unwrap_or_default();
        let truncated = raw_rows.len() > row_limit;
        let take = raw_rows.len().min(row_limit);
        let mut out_rows = Vec::with_capacity(take);
        for r in raw_rows.into_iter().take(take) {
            let mut row = Vec::with_capacity(columns.len());
            if let Some(cells) = r.columns {
                for cell in cells {
                    let s = match cell.value {
                        Some(serde_json::Value::String(s)) => s,
                        Some(serde_json::Value::Null) | None => String::new(),
                        Some(v) => v.to_string(),
                    };
                    row.push(s);
                }
            }
            // Pad short rows so the per-column index stays meaningful.
            while row.len() < columns.len() {
                row.push(String::new());
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
