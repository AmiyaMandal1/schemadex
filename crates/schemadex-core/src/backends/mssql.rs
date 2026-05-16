//! Microsoft SQL Server backend via `tiberius`.
//!
//! Tiberius is a single-connection driver, so we wrap the client in a
//! `tokio::sync::Mutex` and serialize all access. For the agent workloads this
//! crate targets (introspection + the occasional ad-hoc SELECT) that's plenty
//! — and it keeps the dep footprint small. Production users who hammer the
//! same database from many tasks can layer a connection pool on top.

use crate::error::{Result, SchemadexError};
use crate::introspector::{Backend, QueryResult, QueryRunner, SchemaIntrospector};
use crate::model::{Column, DataType, ForeignKey, PrimaryKey};
use async_trait::async_trait;
use std::collections::BTreeMap;
use std::sync::Arc;
use tiberius::{AuthMethod, Client, ColumnData, Config, EncryptionLevel, QueryItem};
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio_util::compat::{Compat, TokioAsyncWriteCompatExt};
use futures::TryStreamExt;

type TibClient = Client<Compat<TcpStream>>;

pub struct MssqlIntrospector {
    pub url: String,
    client: Arc<Mutex<TibClient>>,
}

impl MssqlIntrospector {
    /// Connect using a `mssql://user:pass@host:port/database` URL (or
    /// `sqlserver://`). Maps the URL into a `tiberius::Config` and opens a
    /// single TCP connection. Encryption is set to `NotSupported` by default
    /// so the test container's self-signed cert doesn't break things; callers
    /// who need TLS can use the ADO-style query parameter `encrypt=true`.
    pub async fn connect(url: &str) -> Result<Self> {
        let config = parse_mssql_url(url)?;
        let tcp = TcpStream::connect(config.get_addr())
            .await
            .map_err(|e| SchemadexError::Other(format!("mssql tcp connect: {e}")))?;
        tcp.set_nodelay(true)
            .map_err(|e| SchemadexError::Other(format!("mssql set_nodelay: {e}")))?;
        let client = Client::connect(config, tcp.compat_write())
            .await
            .map_err(|e| SchemadexError::Other(format!("mssql connect: {e}")))?;
        Ok(Self {
            url: url.to_string(),
            client: Arc::new(Mutex::new(client)),
        })
    }
}

/// Parse `mssql://user:pass@host:port/database` (or the `sqlserver://`
/// variant) into a `tiberius::Config`. Falls back to defaults (host
/// `localhost`, port `1433`) when the URL omits them. Query-string params
/// like `?encrypt=true` flip encryption back on.
fn parse_mssql_url(url: &str) -> Result<Config> {
    let parsed = url::Url::parse(url)
        .map_err(|e| SchemadexError::Other(format!("mssql url parse: {e}")))?;
    let scheme = parsed.scheme();
    if scheme != "mssql" && scheme != "sqlserver" {
        return Err(SchemadexError::Other(format!(
            "mssql backend cannot handle scheme `{scheme}`"
        )));
    }
    let mut config = Config::new();
    config.host(parsed.host_str().unwrap_or("localhost"));
    config.port(parsed.port().unwrap_or(1433));
    if !parsed.username().is_empty() {
        let user = parsed.username().to_string();
        let pass = parsed.password().unwrap_or("").to_string();
        config.authentication(AuthMethod::sql_server(&user, &pass));
    }
    let database = parsed.path().trim_start_matches('/');
    if database.is_empty() {
        return Err(SchemadexError::Other(
            "MSSQL URL must include a database: mssql://user:pass@host:port/database".into(),
        ));
    }
    config.database(database);

    // Default to no encryption so localhost/dev containers Just Work. The
    // user can opt back in via `?encrypt=true`.
    let mut encrypt = false;
    let mut trust_cert = true;
    for (k, v) in parsed.query_pairs() {
        match k.as_ref() {
            "encrypt" => encrypt = matches!(v.as_ref(), "true" | "1" | "yes"),
            "trust_server_certificate" | "trust_cert" => {
                trust_cert = matches!(v.as_ref(), "true" | "1" | "yes")
            }
            _ => {}
        }
    }
    if encrypt {
        config.encryption(EncryptionLevel::Required);
    } else {
        config.encryption(EncryptionLevel::NotSupported);
    }
    if trust_cert {
        config.trust_cert();
    }
    Ok(config)
}

/// Map an `information_schema.columns.data_type` value into the coarse
/// [`DataType`] bucket. MSSQL surfaces e.g. `nvarchar`, `varchar(max)`, `int`,
/// `bigint`; we match those plus the temporal and binary variants.
pub fn mssql_classify(native: &str) -> DataType {
    let t = native.to_lowercase();
    if t == "tinyint" || t == "smallint" || t == "int" || t == "bigint" {
        DataType::Integer
    } else if t == "real" || t == "float" {
        DataType::Float
    } else if t == "decimal" || t == "numeric" || t == "money" || t == "smallmoney" {
        DataType::Decimal
    } else if t == "char"
        || t == "varchar"
        || t == "text"
        || t == "nchar"
        || t == "nvarchar"
        || t == "ntext"
        || t == "sysname"
    {
        DataType::Text
    } else if t == "bit" {
        DataType::Bool
    } else if t == "date" {
        DataType::Date
    } else if t == "time" {
        DataType::Time
    } else if t == "datetime"
        || t == "datetime2"
        || t == "smalldatetime"
        || t == "datetimeoffset"
    {
        DataType::Timestamp
    } else if t == "uniqueidentifier" {
        DataType::Uuid
    } else if t == "binary" || t == "varbinary" || t == "image" || t == "rowversion" {
        DataType::Bytes
    } else if t == "xml" || t == "json" {
        DataType::Json
    } else {
        DataType::Unknown
    }
}

/// Stringify a single MSSQL cell. Mirrors the per-backend approach in the
/// postgres/mysql implementations: NULL becomes the empty string, unknown
/// variants fall back to `<unsupported: ...>` rather than panicking.
fn mssql_cell_to_string(value: &ColumnData<'_>) -> String {
    match value {
        ColumnData::U8(Some(v)) => v.to_string(),
        ColumnData::I16(Some(v)) => v.to_string(),
        ColumnData::I32(Some(v)) => v.to_string(),
        ColumnData::I64(Some(v)) => v.to_string(),
        ColumnData::F32(Some(v)) => v.to_string(),
        ColumnData::F64(Some(v)) => v.to_string(),
        ColumnData::Bit(Some(v)) => v.to_string(),
        ColumnData::String(Some(s)) => s.to_string(),
        ColumnData::Guid(Some(g)) => g.to_string(),
        ColumnData::Binary(Some(b)) => format!("<{} bytes>", b.len()),
        ColumnData::Numeric(Some(n)) => n.to_string(),
        ColumnData::U8(None)
        | ColumnData::I16(None)
        | ColumnData::I32(None)
        | ColumnData::I64(None)
        | ColumnData::F32(None)
        | ColumnData::F64(None)
        | ColumnData::Bit(None)
        | ColumnData::String(None)
        | ColumnData::Guid(None)
        | ColumnData::Binary(None)
        | ColumnData::Numeric(None) => String::new(),
        other => format!("<unsupported: {:?}>", std::mem::discriminant(other)),
    }
}

#[async_trait]
impl SchemaIntrospector for MssqlIntrospector {
    fn backend(&self) -> Backend {
        Backend::Mssql
    }

    #[tracing::instrument(level = "debug", name = "mssql.tables", skip(self))]
    async fn tables(&self) -> Result<Vec<(Option<String>, String)>> {
        let sql = "SELECT TABLE_SCHEMA, TABLE_NAME FROM INFORMATION_SCHEMA.TABLES \
                   WHERE TABLE_TYPE = 'BASE TABLE' AND TABLE_CATALOG = DB_NAME() \
                   ORDER BY TABLE_SCHEMA, TABLE_NAME";
        let mut guard = self.client.lock().await;
        let stream = guard
            .simple_query(sql)
            .await
            .map_err(|e| SchemadexError::Other(format!("mssql tables: {e}")))?;
        let rows: Vec<_> = stream
            .into_first_result()
            .await
            .map_err(|e| SchemadexError::Other(format!("mssql tables collect: {e}")))?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            let s: Option<&str> = r.get(0);
            let n: Option<&str> = r.get(1);
            if let Some(name) = n {
                out.push((s.map(str::to_string), name.to_string()));
            }
        }
        Ok(out)
    }

    #[tracing::instrument(level = "debug", name = "mssql.columns", skip(self))]
    async fn columns(&self, schema: Option<&str>, table: &str) -> Result<Vec<Column>> {
        let schema = schema.unwrap_or("dbo");
        let sql = "SELECT COLUMN_NAME, DATA_TYPE, IS_NULLABLE, COLUMN_DEFAULT, ORDINAL_POSITION \
                   FROM INFORMATION_SCHEMA.COLUMNS \
                   WHERE TABLE_SCHEMA = @P1 AND TABLE_NAME = @P2 AND TABLE_CATALOG = DB_NAME() \
                   ORDER BY ORDINAL_POSITION";
        let mut guard = self.client.lock().await;
        let stream = guard
            .query(sql, &[&schema, &table])
            .await
            .map_err(|e| SchemadexError::Other(format!("mssql columns: {e}")))?;
        let rows: Vec<_> = stream
            .into_first_result()
            .await
            .map_err(|e| SchemadexError::Other(format!("mssql columns collect: {e}")))?;
        let mut cols = Vec::with_capacity(rows.len());
        for r in rows {
            let name: &str = r.get(0).unwrap_or("");
            let native: &str = r.get(1).unwrap_or("");
            let nullable: &str = r.get(2).unwrap_or("NO");
            let default: Option<&str> = r.get(3);
            let ordinal: i32 = r.get::<i32, _>(4).unwrap_or(0);
            cols.push(Column {
                name: name.to_string(),
                data_type: mssql_classify(native),
                native_type: native.to_string(),
                nullable: nullable.eq_ignore_ascii_case("yes"),
                default: default.map(str::to_string),
                comment: None,
                ordinal,
                sample: None,
            });
        }
        Ok(cols)
    }

    #[tracing::instrument(level = "debug", name = "mssql.primary_key", skip(self))]
    async fn primary_key(&self, schema: Option<&str>, table: &str) -> Result<Option<PrimaryKey>> {
        let schema = schema.unwrap_or("dbo");
        let sql = "SELECT kcu.CONSTRAINT_NAME, kcu.COLUMN_NAME \
                   FROM INFORMATION_SCHEMA.TABLE_CONSTRAINTS tc \
                   JOIN INFORMATION_SCHEMA.KEY_COLUMN_USAGE kcu \
                     ON tc.CONSTRAINT_NAME = kcu.CONSTRAINT_NAME \
                    AND tc.TABLE_SCHEMA = kcu.TABLE_SCHEMA \
                    AND tc.TABLE_NAME = kcu.TABLE_NAME \
                   WHERE tc.CONSTRAINT_TYPE = 'PRIMARY KEY' \
                     AND tc.TABLE_SCHEMA = @P1 AND tc.TABLE_NAME = @P2 \
                     AND tc.TABLE_CATALOG = DB_NAME() \
                   ORDER BY kcu.ORDINAL_POSITION";
        let mut guard = self.client.lock().await;
        let stream = guard
            .query(sql, &[&schema, &table])
            .await
            .map_err(|e| SchemadexError::Other(format!("mssql pk: {e}")))?;
        let rows: Vec<_> = stream
            .into_first_result()
            .await
            .map_err(|e| SchemadexError::Other(format!("mssql pk collect: {e}")))?;
        if rows.is_empty() {
            return Ok(None);
        }
        let name = rows
            .first()
            .and_then(|r| r.get::<&str, _>(0))
            .map(str::to_string);
        let columns: Vec<String> = rows
            .iter()
            .filter_map(|r| r.get::<&str, _>(1).map(str::to_string))
            .collect();
        Ok(Some(PrimaryKey { name, columns }))
    }

    #[tracing::instrument(level = "debug", name = "mssql.foreign_keys", skip(self))]
    async fn foreign_keys(&self, schema: Option<&str>, table: &str) -> Result<Vec<ForeignKey>> {
        let schema = schema.unwrap_or("dbo");
        // INFORMATION_SCHEMA.REFERENTIAL_CONSTRAINTS gives the (constraint,
        // unique-constraint) pair; we join KEY_COLUMN_USAGE twice — once on the
        // FK side (this table) and once on the referenced side — to get both
        // column lists in order.
        let sql = "SELECT rc.CONSTRAINT_NAME, kcu.COLUMN_NAME, \
                          rkcu.TABLE_NAME AS REF_TABLE, rkcu.COLUMN_NAME AS REF_COLUMN \
                   FROM INFORMATION_SCHEMA.REFERENTIAL_CONSTRAINTS rc \
                   JOIN INFORMATION_SCHEMA.KEY_COLUMN_USAGE kcu \
                     ON kcu.CONSTRAINT_NAME = rc.CONSTRAINT_NAME \
                    AND kcu.TABLE_SCHEMA = rc.CONSTRAINT_SCHEMA \
                   JOIN INFORMATION_SCHEMA.KEY_COLUMN_USAGE rkcu \
                     ON rkcu.CONSTRAINT_NAME = rc.UNIQUE_CONSTRAINT_NAME \
                    AND rkcu.TABLE_SCHEMA = rc.UNIQUE_CONSTRAINT_SCHEMA \
                    AND rkcu.ORDINAL_POSITION = kcu.ORDINAL_POSITION \
                   WHERE kcu.TABLE_SCHEMA = @P1 AND kcu.TABLE_NAME = @P2 \
                     AND kcu.TABLE_CATALOG = DB_NAME() \
                   ORDER BY rc.CONSTRAINT_NAME, kcu.ORDINAL_POSITION";
        let mut guard = self.client.lock().await;
        let stream = guard
            .query(sql, &[&schema, &table])
            .await
            .map_err(|e| SchemadexError::Other(format!("mssql fk: {e}")))?;
        let rows: Vec<_> = stream
            .into_first_result()
            .await
            .map_err(|e| SchemadexError::Other(format!("mssql fk collect: {e}")))?;
        let mut by_name: BTreeMap<String, ForeignKey> = BTreeMap::new();
        for r in rows {
            let cname: &str = r.get(0).unwrap_or("");
            let col: &str = r.get(1).unwrap_or("");
            let ref_tbl: &str = r.get(2).unwrap_or("");
            let ref_col: &str = r.get(3).unwrap_or("");
            let fk = by_name.entry(cname.to_string()).or_insert(ForeignKey {
                name: Some(cname.to_string()),
                columns: vec![],
                referenced_table: ref_tbl.to_string(),
                referenced_columns: vec![],
            });
            fk.columns.push(col.to_string());
            fk.referenced_columns.push(ref_col.to_string());
        }
        Ok(by_name.into_values().collect())
    }
}

#[async_trait]
impl QueryRunner for MssqlIntrospector {
    #[tracing::instrument(
        level = "debug",
        name = "mssql.run_sql",
        skip(self, sql),
        fields(sql_len = sql.len(), row_limit),
    )]
    async fn run_sql(&self, sql: &str, row_limit: usize) -> Result<QueryResult> {
        let mut guard = self.client.lock().await;
        let mut stream = guard
            .simple_query(sql)
            .await
            .map_err(|e| SchemadexError::Other(format!("mssql run_sql: {e}")))?;

        let mut columns: Vec<String> = Vec::new();
        let mut out_rows: Vec<Vec<String>> = Vec::new();
        let mut truncated = false;

        // Iterate over the result stream manually so we can capture the column
        // metadata exactly once (it arrives ahead of any rows) and bail out as
        // soon as we exceed `row_limit + 1`.
        while let Some(item) = stream
            .try_next()
            .await
            .map_err(|e| SchemadexError::Other(format!("mssql run_sql stream: {e}")))?
        {
            match item {
                QueryItem::Metadata(meta) => {
                    if columns.is_empty() {
                        columns = meta
                            .columns()
                            .iter()
                            .map(|c| c.name().to_string())
                            .collect();
                    }
                }
                QueryItem::Row(row) => {
                    if out_rows.len() >= row_limit {
                        truncated = true;
                        continue;
                    }
                    let mut r = Vec::with_capacity(columns.len().max(row.len()));
                    for i in 0..row.len() {
                        let cell = row
                            .try_get::<&str, _>(i)
                            .ok()
                            .flatten()
                            .map(String::from)
                            .or_else(|| {
                                // Fallback: re-encode the typed column into a
                                // string via the ColumnData representation.
                                row.cells().nth(i).map(|(_, c)| mssql_cell_to_string(c))
                            })
                            .unwrap_or_default();
                        r.push(cell);
                    }
                    out_rows.push(r);
                }
            }
        }

        Ok(QueryResult {
            columns,
            rows: out_rows,
            truncated,
        })
    }
}
