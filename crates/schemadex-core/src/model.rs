use serde::{Deserialize, Serialize};

/// Coarse data-type bucket so backends can describe themselves uniformly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DataType {
    Integer,
    Float,
    Decimal,
    Text,
    Bool,
    Date,
    Time,
    Timestamp,
    Json,
    Uuid,
    Bytes,
    Array,
    Unknown,
}

impl DataType {
    /// Returns true if the type is numeric (good candidate for percentile sampling).
    pub fn is_numeric(self) -> bool {
        matches!(
            self,
            DataType::Integer | DataType::Float | DataType::Decimal
        )
    }

    /// Returns true if the type is categorical-ish (good candidate for top-K sampling).
    pub fn is_categorical(self) -> bool {
        matches!(
            self,
            DataType::Text | DataType::Bool | DataType::Uuid | DataType::Date
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Column {
    pub name: String,
    pub data_type: DataType,
    pub native_type: String,
    pub nullable: bool,
    pub default: Option<String>,
    pub comment: Option<String>,
    pub ordinal: i32,
    #[serde(default)]
    pub sample: Option<ColumnSample>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ColumnSample {
    pub stats: SampleStats,
    /// Frequency-sorted (value, fraction) pairs for categorical-ish columns.
    pub top_values: Vec<(String, f32)>,
    /// If a single value dominates above the sentinel threshold, record its fraction.
    pub sentinel: Option<(String, f32)>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct SampleStats {
    pub distinct_count: Option<u64>,
    pub null_fraction: Option<f32>,
    pub min: Option<String>,
    pub max: Option<String>,
    pub p50: Option<String>,
    pub p95: Option<String>,
    pub p99: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PrimaryKey {
    pub name: Option<String>,
    pub columns: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ForeignKey {
    pub name: Option<String>,
    pub columns: Vec<String>,
    pub referenced_table: String,
    pub referenced_columns: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Table {
    pub schema: Option<String>,
    pub name: String,
    pub comment: Option<String>,
    pub columns: Vec<Column>,
    pub primary_key: Option<PrimaryKey>,
    pub foreign_keys: Vec<ForeignKey>,
    pub row_count_estimate: Option<u64>,
    /// Backend-specific DDL hash, populated by [`crate::fingerprint`].
    pub ddl_hash: Option<String>,
}

impl Table {
    pub fn qualified_name(&self) -> String {
        match &self.schema {
            Some(s) => format!("{s}.{}", self.name),
            None => self.name.clone(),
        }
    }

    pub fn column(&self, name: &str) -> Option<&Column> {
        self.columns
            .iter()
            .find(|c| c.name.eq_ignore_ascii_case(name))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct Database {
    pub backend: String,
    pub url_hash: String,
    pub tables: Vec<Table>,
    pub fingerprint: Option<String>,
}

impl Database {
    pub fn table(&self, name: &str) -> Option<&Table> {
        self.tables.iter().find(|t| {
            t.name.eq_ignore_ascii_case(name) || t.qualified_name().eq_ignore_ascii_case(name)
        })
    }

    pub fn list_tables(&self) -> Vec<String> {
        self.tables.iter().map(|t| t.qualified_name()).collect()
    }
}
