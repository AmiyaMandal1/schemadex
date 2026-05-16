//! Project-supplied synonym dictionary layered on top of the fuzzy resolver.
//!
//! Lexical similarity can't capture domain knowledge like `customer == user`
//! or `total == total_cents`. This module loads a YAML file of human-curated
//! aliases and lets resolvers consult it before falling back to fuzzy matching.
//!
//! # YAML format
//!
//! Each top-level key is the *real* table/column name, the value is a list of
//! alternative names. Keys can be:
//! - Bare column names — apply globally (every table)
//! - Table names — apply to table-name resolution
//! - `table.column` — apply only when resolving on that specific table
//!
//! ```yaml
//! users:
//!   - customer
//!   - account
//! orders.total_cents:
//!   - total
//!   - amount
//!   - price
//! ```

use crate::error::{Result, SchemadexError};
use crate::model::Table;
use crate::resolve::{resolve_column, ResolveResult};
use std::collections::BTreeMap;
use std::path::Path;

/// One entry in the synonym map: `alias -> target`, optionally scoped to a
/// specific `table`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SynonymEntry {
    /// Alias the agent might use, lowercased.
    pub alias: String,
    /// `Some(table)` if scoped via `table.column`; `None` for global/table-level.
    pub table: Option<String>,
    /// Real column or table name to resolve to (original casing preserved).
    pub target: String,
}

/// Project-wide synonym map. Cheap to clone (entries are small) and to query
/// (linear over a typically tiny vector — projects supply tens of aliases, not
/// thousands).
#[derive(Debug, Clone, Default)]
pub struct SynonymMap {
    pub entries: Vec<SynonymEntry>,
}

impl SynonymMap {
    /// Parse a YAML synonym file from disk. Returns an empty map if the file
    /// is empty; errors if the file is missing or malformed.
    pub fn load_yaml(path: &Path) -> Result<Self> {
        let contents = std::fs::read_to_string(path).map_err(SchemadexError::Io)?;
        Self::from_yaml_str(&contents)
    }

    /// Parse a YAML synonym map from an in-memory string. Empty / whitespace-
    /// only input yields an empty map.
    pub fn from_yaml_str(s: &str) -> Result<Self> {
        if s.trim().is_empty() {
            return Ok(Self::default());
        }
        // Top level is `{ key: [alias, alias, ...] }`. Use BTreeMap for
        // deterministic iteration order in tests.
        let raw: BTreeMap<String, Vec<String>> = serde_yaml::from_str(s)
            .map_err(|e| SchemadexError::Other(format!("synonyms YAML parse error: {e}")))?;

        let mut entries = Vec::new();
        for (key, aliases) in raw {
            let (table, target) = if let Some((tbl, col)) = key.split_once('.') {
                (Some(tbl.trim().to_string()), col.trim().to_string())
            } else {
                (None, key.trim().to_string())
            };
            for alias in aliases {
                entries.push(SynonymEntry {
                    alias: alias.trim().to_lowercase(),
                    table: table.clone(),
                    target: target.clone(),
                });
            }
        }
        Ok(Self { entries })
    }

    /// Resolve `candidate` (possibly scoped to `table`) to a canonical name
    /// via the synonym map. Returns `None` if no synonym matches.
    ///
    /// Lookup order (first hit wins):
    /// 1. Scoped match: `table.alias -> target` when `table` is supplied.
    /// 2. Global/table-level match: `alias -> target`.
    pub fn resolve(&self, table: Option<&str>, candidate: &str) -> Option<String> {
        let cand_lc = candidate.to_lowercase();

        // Scoped entries first (table-qualified keys win over global).
        if let Some(t) = table {
            let t_lc = t.to_lowercase();
            for e in &self.entries {
                if e.alias == cand_lc
                    && e.table
                        .as_deref()
                        .map(|et| et.eq_ignore_ascii_case(&t_lc))
                        .unwrap_or(false)
                {
                    return Some(e.target.clone());
                }
            }
        }

        // Global (unscoped) entries.
        for e in &self.entries {
            if e.alias == cand_lc && e.table.is_none() {
                return Some(e.target.clone());
            }
        }

        None
    }
}

/// Synonym-aware version of [`resolve_column`]. If `candidate` matches a
/// synonym whose target is a real column on `table`, returns that match with
/// `confidence = 1.0`. Otherwise falls back to the lexical resolver.
pub fn resolve_column_with_synonyms(
    table: &Table,
    candidate: &str,
    synonyms: &SynonymMap,
) -> ResolveResult {
    // Try table-scoped first, then global.
    let scoped_hit = synonyms.resolve(Some(&table.name), candidate);
    let global_hit = synonyms.resolve(None, candidate);

    for hit in [scoped_hit, global_hit].into_iter().flatten() {
        if let Some(col) = table.column(&hit) {
            return ResolveResult {
                matched: Some(col.name.clone()),
                confidence: 1.0,
                alternatives: Vec::new(),
            };
        }
    }

    resolve_column(table, candidate)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Column, DataType, Table};

    fn col(name: &str) -> Column {
        Column {
            name: name.to_string(),
            data_type: DataType::Text,
            native_type: "text".to_string(),
            nullable: true,
            default: None,
            comment: None,
            ordinal: 0,
            sample: None,
            check_constraint: None,
            is_unique: false,
            generation_expression: None,
        }
    }

    fn orders_table() -> Table {
        Table {
            schema: None,
            name: "orders".to_string(),
            comment: None,
            columns: vec![col("id"), col("total_cents"), col("created_at")],
            primary_key: None,
            foreign_keys: vec![],
            row_count_estimate: None,
            ddl_hash: None,
        }
    }

    #[test]
    fn load_global_alias() {
        let map = SynonymMap::from_yaml_str("users:\n  - customer\n").unwrap();
        assert_eq!(map.resolve(None, "customer"), Some("users".to_string()));
        // Case-insensitive match.
        assert_eq!(map.resolve(None, "Customer"), Some("users".to_string()));
        // Non-matches return None.
        assert_eq!(map.resolve(None, "purchaser"), None);
    }

    #[test]
    fn load_scoped_alias() {
        let map = SynonymMap::from_yaml_str("orders.total_cents:\n  - total\n").unwrap();
        assert_eq!(
            map.resolve(Some("orders"), "total"),
            Some("total_cents".to_string())
        );
        // Wrong table: scoped entry must not leak across tables.
        assert_eq!(map.resolve(Some("customers"), "total"), None);
        // No table context: scoped entry should not match.
        assert_eq!(map.resolve(None, "total"), None);
    }

    #[test]
    fn resolve_column_with_synonyms_hits_synonym_first() {
        let table = orders_table();
        let map = SynonymMap::from_yaml_str("orders.total_cents:\n  - total\n").unwrap();
        let r = resolve_column_with_synonyms(&table, "total", &map);
        assert_eq!(r.matched.as_deref(), Some("total_cents"));
        assert!((r.confidence - 1.0).abs() < 1e-6);
        assert!(r.alternatives.is_empty());
    }

    #[test]
    fn resolve_column_with_synonyms_falls_back_to_fuzzy() {
        let table = orders_table();
        let map = SynonymMap::default();
        // No synonyms — must behave like resolve_column.
        let r = resolve_column_with_synonyms(&table, "totalcents", &map);
        assert_eq!(r.matched.as_deref(), Some("total_cents"));
    }

    #[test]
    fn empty_yaml_yields_empty_map() {
        let map = SynonymMap::from_yaml_str("").unwrap();
        assert!(map.entries.is_empty());
        let map = SynonymMap::from_yaml_str("   \n  \n").unwrap();
        assert!(map.entries.is_empty());
    }

    #[test]
    fn synonym_target_must_be_real_column() {
        // If the synonym points to a column that doesn't exist on this table,
        // fall back to fuzzy rather than returning a phantom match.
        let table = orders_table();
        let map = SynonymMap::from_yaml_str("orders.nonexistent:\n  - foo\n").unwrap();
        let r = resolve_column_with_synonyms(&table, "foo", &map);
        // Falls back to fuzzy — best lexical match on "foo" is whichever the
        // resolver picks, but confidence should not be 1.0.
        assert!(r.confidence < 1.0);
    }

    #[test]
    fn scoped_wins_over_global() {
        // If both a global and a scoped entry exist for the same alias on the
        // given table, the scoped one wins.
        let yaml = "orders.total_cents:\n  - amount\nfees:\n  - amount\n";
        let map = SynonymMap::from_yaml_str(yaml).unwrap();
        assert_eq!(
            map.resolve(Some("orders"), "amount"),
            Some("total_cents".to_string())
        );
        // From an unrelated table, the global entry kicks in.
        assert_eq!(
            map.resolve(Some("other_table"), "amount"),
            Some("fees".to_string())
        );
    }
}
