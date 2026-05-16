use thiserror::Error;

#[derive(Debug, Error)]
pub enum SchemadexError {
    #[error("backend `{0}` is not compiled in. Enable the matching feature flag.")]
    BackendDisabled(&'static str),

    #[error("unsupported database URL scheme: {0}")]
    UnsupportedScheme(String),

    #[error("table not found: {0}")]
    TableNotFound(String),

    #[error("column `{column}` not found on table `{table}`")]
    ColumnNotFound { table: String, column: String },

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("url parse error: {0}")]
    Url(#[from] url::ParseError),

    #[cfg(any(feature = "postgres", feature = "sqlite"))]
    #[error("sqlx error: {0}")]
    Sqlx(#[from] sqlx::Error),

    #[cfg(feature = "duckdb_backend")]
    #[error("duckdb error: {0}")]
    DuckDb(#[from] duckdb::Error),

    #[error("token budget exceeded: needed {needed}, budget {budget}")]
    TokenBudget { needed: usize, budget: usize },

    #[error("{0}")]
    Other(String),
}

pub type Result<T, E = SchemadexError> = std::result::Result<T, E>;
