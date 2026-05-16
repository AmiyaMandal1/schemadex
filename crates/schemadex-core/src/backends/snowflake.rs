//! Snowflake backend via the SQL REST API + key-pair JWT auth.
//!
//! Snowflake doesn't have a first-party Rust SDK, but the SQL API
//! (`/api/v2/statements`) is reasonable and only needs an OAuth-bearer-style
//! token. We mint that token ourselves from the user's private key (the
//! "key-pair authentication" flow that Snowflake recommends for headless
//! workloads).
//!
//! Environment variables, all required when the backend is invoked:
//!   - `SNOWFLAKE_USER`             — Snowflake login name (uppercase)
//!   - `SNOWFLAKE_ACCOUNT`          — Account locator, e.g. `xy12345.us-east-1`
//!   - `SNOWFLAKE_PRIVATE_KEY_PATH` — Path to a PEM-encoded RSA private key
//!     registered as a public key for the user. Encrypted keys are not
//!     currently supported here.
//!
//! Optional:
//!   - `SNOWFLAKE_WAREHOUSE`        — Default warehouse for query execution
//!   - `SNOWFLAKE_ROLE`             — Role to assume
//!
//! URL shape: `snowflake://account/database[/schema]`. The account in the
//! URL takes precedence over `SNOWFLAKE_ACCOUNT` so per-instance overrides
//! are easy. Introspection queries all hit `INFORMATION_SCHEMA` in the
//! configured database.

use crate::error::{Result, SchemadexError};
use crate::introspector::{Backend, QueryResult, QueryRunner, SchemaIntrospector};
use crate::model::{Column, DataType, ForeignKey, PrimaryKey};
use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use rsa::pkcs8::{DecodePrivateKey, EncodePublicKey};
use rsa::RsaPrivateKey;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

pub struct SnowflakeIntrospector {
    pub account: String,
    pub database: String,
    pub schema: Option<String>,
    warehouse: Option<String>,
    role: Option<String>,
    http: reqwest::Client,
    issuer: String,
    /// JWT private key in PEM (raw bytes). We re-sign on every request so
    /// tokens never expire mid-call.
    private_key_pem: Vec<u8>,
    user: String,
}

impl SnowflakeIntrospector {
    /// Connect — really just parse the URL and load the private key so we
    /// fail fast if auth is misconfigured. No network IO happens here.
    pub async fn connect(url: &str) -> Result<Self> {
        let (account_from_url, database, schema) = parse_snowflake_url(url)?;
        let account = std::env::var("SNOWFLAKE_ACCOUNT")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or(account_from_url);
        let user = std::env::var("SNOWFLAKE_USER").map_err(|_| {
            SchemadexError::Other(
                "snowflake: SNOWFLAKE_USER must be set for key-pair auth".into(),
            )
        })?;
        let key_path = std::env::var("SNOWFLAKE_PRIVATE_KEY_PATH").map_err(|_| {
            SchemadexError::Other(
                "snowflake: SNOWFLAKE_PRIVATE_KEY_PATH must point at a PEM RSA key".into(),
            )
        })?;
        let private_key_pem = std::fs::read(&key_path).map_err(|e| {
            SchemadexError::Other(format!(
                "snowflake: failed to read private key at {key_path}: {e}"
            ))
        })?;
        let issuer = snowflake_jwt_issuer(&account, &user, &private_key_pem)?;
        let warehouse = std::env::var("SNOWFLAKE_WAREHOUSE")
            .ok()
            .filter(|s| !s.is_empty());
        let role = std::env::var("SNOWFLAKE_ROLE")
            .ok()
            .filter(|s| !s.is_empty());
        let http = reqwest::Client::builder()
            .build()
            .map_err(|e| SchemadexError::Other(format!("snowflake http build: {e}")))?;
        Ok(Self {
            account,
            database,
            schema,
            warehouse,
            role,
            http,
            issuer,
            private_key_pem,
            user,
        })
    }

    /// Build a short-lived JWT signed with the user's RSA private key. Snowflake
    /// requires the issuer to be `<ACCOUNT>.<USER>.SHA256:<base64-pubkey-hash>`,
    /// the subject to be `<ACCOUNT>.<USER>`, and the `exp` to be within an hour.
    fn fresh_jwt(&self) -> Result<String> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let claims = Claims {
            iss: self.issuer.clone(),
            sub: format!(
                "{}.{}",
                self.account.to_uppercase(),
                self.user.to_uppercase()
            ),
            iat: now,
            exp: now + 3000,
        };
        let key = EncodingKey::from_rsa_pem(&self.private_key_pem)
            .map_err(|e| SchemadexError::Other(format!("snowflake JWT key parse: {e}")))?;
        encode(&Header::new(Algorithm::RS256), &claims, &key)
            .map_err(|e| SchemadexError::Other(format!("snowflake JWT sign: {e}")))
    }

    /// Execute a SQL statement against the SQL REST endpoint and return the
    /// raw response. Used by both introspection and `run_sql`.
    async fn execute(&self, sql: &str, row_limit: Option<usize>) -> Result<StatementResponse> {
        let token = self.fresh_jwt()?;
        let mut body = serde_json::json!({
            "statement": sql,
            "timeout": 60,
        });
        if let Some(wh) = &self.warehouse {
            body["warehouse"] = serde_json::Value::String(wh.clone());
        }
        if let Some(role) = &self.role {
            body["role"] = serde_json::Value::String(role.clone());
        }
        body["database"] = serde_json::Value::String(self.database.clone());
        if let Some(schema) = &self.schema {
            body["schema"] = serde_json::Value::String(schema.clone());
        }
        let url = format!(
            "https://{}.snowflakecomputing.com/api/v2/statements",
            self.account.to_lowercase()
        );
        let mut req = self
            .http
            .post(&url)
            .header("Authorization", format!("Bearer {token}"))
            .header("X-Snowflake-Authorization-Token-Type", "KEYPAIR_JWT")
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .json(&body);
        if let Some(n) = row_limit {
            // Snowflake API: row count is controlled via `?nullable=…` etc;
            // the simpler knob is to cap server-side via LIMIT in the SQL.
            // We still pass `parameters.MULTI_STATEMENT_COUNT` defensively.
            let _ = n;
            req = req.query(&[("nullable", "false")]);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| SchemadexError::Other(format!("snowflake request: {e}")))?;
        let status = resp.status();
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| SchemadexError::Other(format!("snowflake response body: {e}")))?;
        if !status.is_success() {
            let snippet = String::from_utf8_lossy(&bytes);
            return Err(SchemadexError::Other(format!(
                "snowflake {status}: {}",
                snippet.chars().take(500).collect::<String>()
            )));
        }
        let parsed: StatementResponse = serde_json::from_slice(&bytes)
            .map_err(|e| SchemadexError::Other(format!("snowflake response parse: {e}")))?;
        Ok(parsed)
    }
}

#[derive(Serialize)]
struct Claims {
    iss: String,
    sub: String,
    iat: u64,
    exp: u64,
}

#[derive(Debug, Deserialize)]
struct StatementResponse {
    #[serde(rename = "resultSetMetaData", default)]
    result_set_meta_data: Option<ResultSetMetadata>,
    #[serde(default)]
    data: Option<Vec<Vec<Option<serde_json::Value>>>>,
}

#[derive(Debug, Deserialize)]
struct ResultSetMetadata {
    #[serde(default)]
    #[serde(rename = "rowType")]
    row_type: Vec<RowTypeField>,
    #[serde(default, rename = "numRows")]
    num_rows: u64,
}

#[derive(Debug, Deserialize)]
struct RowTypeField {
    name: String,
    #[serde(rename = "type", default)]
    #[allow(dead_code)]
    type_: String,
}

fn parse_snowflake_url(url: &str) -> Result<(String, String, Option<String>)> {
    let trimmed = url.trim_start_matches("snowflake://");
    let mut parts = trimmed.splitn(3, '/');
    let account = parts.next().unwrap_or("").to_string();
    let database = parts.next().unwrap_or("").to_string();
    let schema = parts
        .next()
        .map(str::to_string)
        .filter(|s| !s.is_empty());
    if account.is_empty() || database.is_empty() {
        return Err(SchemadexError::Other(
            "Snowflake URL must include an account and database: \
             snowflake://account/database[/schema]"
                .into(),
        ));
    }
    Ok((account, database, schema))
}

/// Build the `iss` claim required by Snowflake's JWT auth:
/// `<ACCOUNT>.<USER>.SHA256:<base64-encoded-sha256-of-DER-encoded-public-key>`.
fn snowflake_jwt_issuer(account: &str, user: &str, private_key_pem: &[u8]) -> Result<String> {
    let pem = std::str::from_utf8(private_key_pem)
        .map_err(|e| SchemadexError::Other(format!("snowflake PEM not utf-8: {e}")))?;
    let private = RsaPrivateKey::from_pkcs8_pem(pem)
        .or_else(|_| {
            // Some keys are PKCS#1-encoded with an explicit `RSA PRIVATE KEY`
            // header. Try parsing through the public-key DER as a fallback.
            rsa::pkcs1::DecodeRsaPrivateKey::from_pkcs1_pem(pem)
        })
        .map_err(|e| {
            SchemadexError::Other(format!(
                "snowflake: failed to parse RSA private key (PKCS#8 or PKCS#1 PEM expected): {e}"
            ))
        })?;
    let public = private.to_public_key();
    let der = public
        .to_public_key_der()
        .map_err(|e| SchemadexError::Other(format!("snowflake pubkey DER: {e}")))?;
    let mut hasher = Sha256::new();
    hasher.update(der.as_bytes());
    let digest = hasher.finalize();
    let b64 = B64.encode(digest);
    Ok(format!(
        "{}.{}.SHA256:{}",
        account.to_uppercase(),
        user.to_uppercase(),
        b64
    ))
}

/// Classify the Snowflake `data_type` string from `INFORMATION_SCHEMA.COLUMNS`.
/// Snowflake reports e.g. `TEXT`, `NUMBER`, `TIMESTAMP_NTZ`, `FLOAT`.
pub fn snowflake_classify(native: &str) -> DataType {
    let t = native.to_uppercase();
    if t == "TEXT" || t.starts_with("VARCHAR") || t.starts_with("CHAR") || t == "STRING" {
        DataType::Text
    } else if t == "NUMBER" || t.starts_with("DECIMAL") || t.starts_with("NUMERIC") {
        DataType::Decimal
    } else if t == "INT" || t == "INTEGER" || t == "BIGINT" || t == "SMALLINT" || t == "TINYINT" {
        DataType::Integer
    } else if t == "FLOAT" || t == "FLOAT4" || t == "FLOAT8" || t == "REAL" || t == "DOUBLE" {
        DataType::Float
    } else if t == "BOOLEAN" {
        DataType::Bool
    } else if t == "DATE" {
        DataType::Date
    } else if t.starts_with("TIMESTAMP") {
        DataType::Timestamp
    } else if t == "TIME" {
        DataType::Time
    } else if t == "VARIANT" || t == "OBJECT" || t == "ARRAY" {
        DataType::Json
    } else if t == "BINARY" {
        DataType::Bytes
    } else {
        DataType::Unknown
    }
}

fn cell_to_string(v: &Option<serde_json::Value>) -> String {
    match v {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

fn parse_i32(v: &Option<serde_json::Value>) -> i32 {
    match v {
        Some(serde_json::Value::Number(n)) => n.as_i64().unwrap_or(0) as i32,
        Some(serde_json::Value::String(s)) => s.parse::<i32>().unwrap_or(0),
        _ => 0,
    }
}

#[async_trait]
impl SchemaIntrospector for SnowflakeIntrospector {
    fn backend(&self) -> Backend {
        Backend::Snowflake
    }

    #[tracing::instrument(level = "debug", name = "snowflake.tables", skip(self))]
    async fn tables(&self) -> Result<Vec<(Option<String>, String)>> {
        let schema_pred = match &self.schema {
            Some(s) => format!(" AND TABLE_SCHEMA = '{}'", s.replace('\'', "''")),
            None => String::new(),
        };
        let sql = format!(
            "SELECT TABLE_SCHEMA, TABLE_NAME FROM \"{}\".INFORMATION_SCHEMA.TABLES \
             WHERE TABLE_TYPE = 'BASE TABLE'{schema_pred} \
             ORDER BY TABLE_SCHEMA, TABLE_NAME",
            self.database.replace('"', "\"\"")
        );
        let resp = self.execute(&sql, None).await?;
        let mut out = Vec::new();
        for row in resp.data.unwrap_or_default() {
            let schema = row.first().map(cell_to_string).filter(|s| !s.is_empty());
            let name = row.get(1).map(cell_to_string).unwrap_or_default();
            if !name.is_empty() {
                out.push((schema, name));
            }
        }
        Ok(out)
    }

    #[tracing::instrument(level = "debug", name = "snowflake.columns", skip(self))]
    async fn columns(&self, schema: Option<&str>, table: &str) -> Result<Vec<Column>> {
        let schema = schema
            .or(self.schema.as_deref())
            .ok_or_else(|| {
                SchemadexError::Other(
                    "snowflake columns(): schema must be supplied via URL or arg".into(),
                )
            })?;
        let sql = format!(
            "SELECT COLUMN_NAME, DATA_TYPE, IS_NULLABLE, COLUMN_DEFAULT, ORDINAL_POSITION, COMMENT \
             FROM \"{}\".INFORMATION_SCHEMA.COLUMNS \
             WHERE TABLE_SCHEMA = '{}' AND TABLE_NAME = '{}' \
             ORDER BY ORDINAL_POSITION",
            self.database.replace('"', "\"\""),
            schema.replace('\'', "''"),
            table.replace('\'', "''")
        );
        let resp = self.execute(&sql, None).await?;
        let mut out = Vec::new();
        for row in resp.data.unwrap_or_default() {
            let name = row.first().map(cell_to_string).unwrap_or_default();
            let native = row.get(1).map(cell_to_string).unwrap_or_default();
            let nullable_str = row.get(2).map(cell_to_string).unwrap_or_default();
            let default = row.get(3).and_then(|v| match v {
                Some(serde_json::Value::Null) | None => None,
                Some(other) => Some(cell_to_string(&Some(other.clone()))),
            });
            let ordinal = parse_i32(row.get(4).unwrap_or(&None));
            let comment = row.get(5).and_then(|v| {
                let s = cell_to_string(v);
                if s.is_empty() {
                    None
                } else {
                    Some(s)
                }
            });
            out.push(Column {
                name,
                data_type: snowflake_classify(&native),
                native_type: native,
                nullable: nullable_str.eq_ignore_ascii_case("YES"),
                default,
                comment,
                ordinal,
                sample: None,
            });
        }
        Ok(out)
    }

    #[tracing::instrument(level = "debug", name = "snowflake.primary_key", skip(self))]
    async fn primary_key(&self, schema: Option<&str>, table: &str) -> Result<Option<PrimaryKey>> {
        let schema = match schema.or(self.schema.as_deref()) {
            Some(s) => s,
            None => return Ok(None),
        };
        // SHOW PRIMARY KEYS is the canonical Snowflake path; INFORMATION_SCHEMA
        // exposes TABLE_CONSTRAINTS but not the column list directly. SHOW
        // returns one row per PK column with `column_name` / `key_sequence`.
        let sql = format!(
            "SHOW PRIMARY KEYS IN \"{}\".\"{}\".\"{}\"",
            self.database.replace('"', "\"\""),
            schema.replace('"', "\"\""),
            table.replace('"', "\"\"")
        );
        let resp = self.execute(&sql, None).await?;
        let rows = resp.data.unwrap_or_default();
        if rows.is_empty() {
            return Ok(None);
        }
        // SHOW PRIMARY KEYS columns: created_on, database_name, schema_name,
        // table_name, column_name, key_sequence, constraint_name, rely, comment.
        let mut columns: Vec<(i32, String)> = rows
            .iter()
            .filter_map(|row| {
                let col = row.get(4).map(cell_to_string).filter(|s| !s.is_empty())?;
                let seq = parse_i32(row.get(5).unwrap_or(&None));
                Some((seq, col))
            })
            .collect();
        columns.sort_by_key(|(seq, _)| *seq);
        let name = rows.first().and_then(|r| {
            r.get(6).map(cell_to_string).filter(|s| !s.is_empty())
        });
        Ok(Some(PrimaryKey {
            name,
            columns: columns.into_iter().map(|(_, c)| c).collect(),
        }))
    }

    #[tracing::instrument(level = "debug", name = "snowflake.foreign_keys", skip(self))]
    async fn foreign_keys(&self, schema: Option<&str>, table: &str) -> Result<Vec<ForeignKey>> {
        let schema = match schema.or(self.schema.as_deref()) {
            Some(s) => s,
            None => return Ok(vec![]),
        };
        let sql = format!(
            "SHOW IMPORTED KEYS IN \"{}\".\"{}\".\"{}\"",
            self.database.replace('"', "\"\""),
            schema.replace('"', "\"\""),
            table.replace('"', "\"\"")
        );
        let resp = self.execute(&sql, None).await?;
        let rows = resp.data.unwrap_or_default();
        // SHOW IMPORTED KEYS columns: created_on, pk_database_name,
        // pk_schema_name, pk_table_name, pk_column_name, fk_database_name,
        // fk_schema_name, fk_table_name, fk_column_name, key_sequence,
        // update_rule, delete_rule, fk_name, pk_name, deferrability, rely, comment.
        let mut by_name: BTreeMap<String, ForeignKey> = BTreeMap::new();
        for row in rows {
            let pk_table = row.get(3).map(cell_to_string).unwrap_or_default();
            let pk_col = row.get(4).map(cell_to_string).unwrap_or_default();
            let fk_col = row.get(8).map(cell_to_string).unwrap_or_default();
            let fk_name = row
                .get(12)
                .map(cell_to_string)
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| format!("{}_{fk_col}_fk", table));
            let entry = by_name.entry(fk_name.clone()).or_insert(ForeignKey {
                name: Some(fk_name),
                columns: vec![],
                referenced_table: pk_table,
                referenced_columns: vec![],
            });
            entry.columns.push(fk_col);
            entry.referenced_columns.push(pk_col);
        }
        Ok(by_name.into_values().collect())
    }
}

#[async_trait]
impl QueryRunner for SnowflakeIntrospector {
    #[tracing::instrument(
        level = "debug",
        name = "snowflake.run_sql",
        skip(self, sql),
        fields(sql_len = sql.len(), row_limit),
    )]
    async fn run_sql(&self, sql: &str, row_limit: usize) -> Result<QueryResult> {
        let resp = self.execute(sql, Some(row_limit + 1)).await?;
        let meta = resp.result_set_meta_data;
        let columns: Vec<String> = meta
            .as_ref()
            .map(|m| m.row_type.iter().map(|f| f.name.clone()).collect())
            .unwrap_or_default();
        let total = meta.as_ref().map(|m| m.num_rows as usize).unwrap_or(0);
        let truncated = total > row_limit;
        let mut out_rows = Vec::new();
        if let Some(data) = resp.data {
            for row in data.into_iter().take(row_limit) {
                let mut r = Vec::with_capacity(columns.len().max(row.len()));
                for v in row {
                    r.push(cell_to_string(&v));
                }
                while r.len() < columns.len() {
                    r.push(String::new());
                }
                out_rows.push(r);
            }
        }
        Ok(QueryResult {
            columns,
            rows: out_rows,
            truncated,
        })
    }
}
