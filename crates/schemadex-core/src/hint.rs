//! Error-to-hint wrapping.
//!
//! When a database returns a raw error message (e.g. `column "emial" does not
//! exist`) the agent typically retries by guessing. [`hint_for_error`] turns
//! such raw messages into a structured [`ErrorHint`] that points at the
//! likely-real identifier, cutting the retry loop.
//!
//! The function returns `None` if it doesn't recognise the error text — the
//! caller can then surface the original message unchanged.

use crate::model::Database;
use crate::resolve::resolve_column;
use regex::Regex;
use serde::Serialize;
use std::sync::OnceLock;

/// A structured suggestion derived from a database error message.
#[derive(Debug, Clone, Serialize)]
pub struct ErrorHint {
    pub kind: HintKind,
    pub original_identifier: String,
    pub suggested_identifier: Option<String>,
    pub confidence: Option<f32>,
    pub human_message: String,
}

/// What sort of identifier the database complained about.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HintKind {
    /// `column "X" does not exist` / `no such column: X` /
    /// `Unknown column 'X' in 'field list'`. If we could attribute the
    /// column to a specific table the `table` field carries it; otherwise
    /// `None`.
    UnknownColumn { table: Option<String> },
    /// `relation "X" does not exist` / `no such table: X` /
    /// `Table 'X' doesn't exist`.
    UnknownTable,
    /// `column reference "X" is ambiguous`.
    AmbiguousColumn,
}

/// Match an error message against a small library of common Postgres /
/// SQLite / MySQL patterns and produce a structured hint pointing at the
/// likely-correct identifier in the cached `db`. Returns `None` if no
/// pattern matched — callers should fall through to surfacing the raw error.
pub fn hint_for_error(db: &Database, error_message: &str) -> Option<ErrorHint> {
    if let Some(hit) = match_unknown_column(error_message) {
        return Some(build_column_hint(db, &hit.identifier, hit.table.as_deref()));
    }
    if let Some(identifier) = match_unknown_table(error_message) {
        return Some(build_table_hint(db, &identifier));
    }
    if let Some(identifier) = match_ambiguous_column(error_message) {
        return Some(build_ambiguous_hint(db, &identifier));
    }
    None
}

struct ColumnHit {
    identifier: String,
    table: Option<String>,
}

fn match_unknown_column(msg: &str) -> Option<ColumnHit> {
    // Postgres: column "X" does not exist
    static PG: OnceLock<Regex> = OnceLock::new();
    let pg = PG.get_or_init(|| {
        Regex::new(r#"(?i)column\s+"([^"]+)"\s+does\s+not\s+exist"#).expect("pg col regex")
    });
    if let Some(caps) = pg.captures(msg) {
        return Some(ColumnHit {
            identifier: caps[1].to_string(),
            table: None,
        });
    }

    // SQLite: no such column: X        (X may be `t.col` or `col`)
    static SQLITE: OnceLock<Regex> = OnceLock::new();
    let sq = SQLITE.get_or_init(|| {
        Regex::new(r#"(?i)no\s+such\s+column:\s*([A-Za-z_][A-Za-z0-9_.]*)"#).expect("sqlite col regex")
    });
    if let Some(caps) = sq.captures(msg) {
        let raw = caps[1].to_string();
        let (table, col) = split_qualified(&raw);
        return Some(ColumnHit {
            identifier: col,
            table,
        });
    }

    // MySQL: Unknown column 'X' in 'field list'  (X may be `t.col` or `col`)
    static MYSQL: OnceLock<Regex> = OnceLock::new();
    let my = MYSQL.get_or_init(|| {
        Regex::new(r#"(?i)Unknown\s+column\s+'([^']+)'"#).expect("mysql col regex")
    });
    if let Some(caps) = my.captures(msg) {
        let raw = caps[1].to_string();
        let (table, col) = split_qualified(&raw);
        return Some(ColumnHit {
            identifier: col,
            table,
        });
    }

    None
}

fn match_unknown_table(msg: &str) -> Option<String> {
    // Postgres: relation "X" does not exist
    static PG: OnceLock<Regex> = OnceLock::new();
    let pg = PG.get_or_init(|| {
        Regex::new(r#"(?i)relation\s+"([^"]+)"\s+does\s+not\s+exist"#).expect("pg tbl regex")
    });
    if let Some(caps) = pg.captures(msg) {
        return Some(caps[1].to_string());
    }

    // SQLite: no such table: X
    static SQLITE: OnceLock<Regex> = OnceLock::new();
    let sq = SQLITE.get_or_init(|| {
        Regex::new(r#"(?i)no\s+such\s+table:\s*([A-Za-z_][A-Za-z0-9_.]*)"#).expect("sqlite tbl regex")
    });
    if let Some(caps) = sq.captures(msg) {
        return Some(caps[1].to_string());
    }

    // MySQL: Table 'X' doesn't exist
    static MYSQL: OnceLock<Regex> = OnceLock::new();
    let my = MYSQL.get_or_init(|| {
        Regex::new(r#"(?i)Table\s+'([^']+)'\s+doesn't\s+exist"#).expect("mysql tbl regex")
    });
    if let Some(caps) = my.captures(msg) {
        // MySQL prefixes the schema (`mydb.userz`) — strip it for matching.
        let raw = caps[1].to_string();
        let last = raw.rsplit('.').next().unwrap_or(&raw).to_string();
        return Some(last);
    }

    None
}

fn match_ambiguous_column(msg: &str) -> Option<String> {
    // Postgres: column reference "X" is ambiguous
    static PG: OnceLock<Regex> = OnceLock::new();
    let pg = PG.get_or_init(|| {
        Regex::new(r#"(?i)column\s+reference\s+"([^"]+)"\s+is\s+ambiguous"#)
            .expect("pg ambiguous regex")
    });
    if let Some(caps) = pg.captures(msg) {
        return Some(caps[1].to_string());
    }
    None
}

/// Split a `table.column` qualified reference. Returns `(table, column)`.
fn split_qualified(raw: &str) -> (Option<String>, String) {
    match raw.rsplit_once('.') {
        Some((t, c)) => (Some(t.to_string()), c.to_string()),
        None => (None, raw.to_string()),
    }
}

fn build_column_hint(db: &Database, identifier: &str, table: Option<&str>) -> ErrorHint {
    // If we have an explicit table hint, look there. Otherwise search every
    // table and pick the best match overall.
    let (suggested_table, matched, confidence) = match table {
        Some(t) => {
            if let Some(tbl) = db.table(t) {
                let r = resolve_column(tbl, identifier);
                (Some(tbl.name.clone()), r.matched, r.confidence)
            } else {
                (None, None, 0.0)
            }
        }
        None => find_best_column_across_db(db, identifier),
    };

    let (suggestion_clean, confidence_clean) = if matched.is_some() && confidence >= 0.6 {
        (matched.clone(), Some(confidence))
    } else {
        (None, None)
    };

    let human_message = match (&suggestion_clean, &suggested_table) {
        (Some(s), Some(t)) => format!(
            "column '{identifier}' does not exist on table '{t}' — did you mean '{s}'?",
        ),
        (Some(s), None) => format!(
            "column '{identifier}' does not exist — did you mean '{s}'?",
        ),
        (None, Some(t)) => format!("column '{identifier}' does not exist on table '{t}'."),
        (None, None) => format!("column '{identifier}' does not exist."),
    };

    ErrorHint {
        kind: HintKind::UnknownColumn {
            table: suggested_table,
        },
        original_identifier: identifier.to_string(),
        suggested_identifier: suggestion_clean,
        confidence: confidence_clean,
        human_message,
    }
}

fn find_best_column_across_db(
    db: &Database,
    identifier: &str,
) -> (Option<String>, Option<String>, f32) {
    let mut best: Option<(String, String, f32)> = None; // (table, col, score)
    for table in &db.tables {
        let r = resolve_column(table, identifier);
        if let Some(name) = r.matched {
            let score = r.confidence;
            if best.as_ref().map(|(_, _, s)| score > *s).unwrap_or(true) {
                best = Some((table.name.clone(), name, score));
            }
        }
    }
    match best {
        Some((t, c, s)) => (Some(t), Some(c), s),
        None => (None, None, 0.0),
    }
}

fn build_table_hint(db: &Database, identifier: &str) -> ErrorHint {
    let suggestion = fuzzy_table(db, identifier);
    let (suggested_identifier, confidence) = match suggestion {
        Some((n, s)) => (Some(n), Some(s)),
        None => (None, None),
    };
    let human_message = match &suggested_identifier {
        Some(s) => format!("table '{identifier}' does not exist — did you mean '{s}'?"),
        None => format!("table '{identifier}' does not exist."),
    };
    ErrorHint {
        kind: HintKind::UnknownTable,
        original_identifier: identifier.to_string(),
        suggested_identifier,
        confidence,
        human_message,
    }
}

fn build_ambiguous_hint(_db: &Database, identifier: &str) -> ErrorHint {
    let human_message = format!(
        "column '{identifier}' is ambiguous — qualify it with a table or alias prefix (e.g. `users.{identifier}`).",
    );
    ErrorHint {
        kind: HintKind::AmbiguousColumn,
        original_identifier: identifier.to_string(),
        suggested_identifier: None,
        confidence: None,
        human_message,
    }
}

fn fuzzy_table(db: &Database, candidate: &str) -> Option<(String, f32)> {
    if db.tables.is_empty() {
        return None;
    }
    let cand_norm = normalize(candidate);
    let mut scored: Vec<(&str, f32)> = db
        .tables
        .iter()
        .map(|t| {
            let name_norm = normalize(&t.name);
            let jw = strsim::jaro_winkler(&cand_norm, &name_norm) as f32;
            let bonus = if name_norm.contains(&cand_norm) || cand_norm.contains(&name_norm) {
                0.05
            } else {
                0.0
            };
            (t.name.as_str(), (jw + bonus).min(1.0))
        })
        .collect();
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let (name, score) = scored[0];
    if score < 0.6 {
        return None;
    }
    Some((name.to_string(), score))
}

fn normalize(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Column, DataType, Database, Table};

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
        }
    }

    fn users_db() -> Database {
        Database {
            backend: "test".to_string(),
            url_hash: "x".to_string(),
            tables: vec![Table {
                schema: None,
                name: "users".to_string(),
                comment: None,
                columns: vec![col("id"), col("email")],
                primary_key: None,
                foreign_keys: vec![],
                row_count_estimate: None,
                ddl_hash: None,
            }],
            fingerprint: None,
        }
    }

    #[test]
    fn unknown_column_postgres() {
        let db = users_db();
        let hint = hint_for_error(&db, r#"column "emial" does not exist"#).expect("hint");
        match hint.kind {
            HintKind::UnknownColumn { .. } => {}
            _ => panic!("expected UnknownColumn"),
        }
        assert_eq!(hint.original_identifier, "emial");
        assert_eq!(hint.suggested_identifier.as_deref(), Some("email"));
        assert!(hint.human_message.contains("email"));
    }

    #[test]
    fn unknown_table_sqlite() {
        let db = users_db();
        let hint = hint_for_error(&db, "no such table: userz").expect("hint");
        match hint.kind {
            HintKind::UnknownTable => {}
            _ => panic!("expected UnknownTable"),
        }
        assert_eq!(hint.original_identifier, "userz");
        assert_eq!(hint.suggested_identifier.as_deref(), Some("users"));
    }

    #[test]
    fn nothing_matches_returns_none() {
        let db = users_db();
        assert!(hint_for_error(&db, "connection refused").is_none());
    }

    #[test]
    fn unknown_column_mysql_qualified() {
        let db = users_db();
        let hint = hint_for_error(
            &db,
            "Unknown column 'users.emial' in 'field list'",
        )
        .expect("hint");
        match &hint.kind {
            HintKind::UnknownColumn { table } => {
                assert_eq!(table.as_deref(), Some("users"));
            }
            _ => panic!("expected UnknownColumn"),
        }
        assert_eq!(hint.suggested_identifier.as_deref(), Some("email"));
    }

    #[test]
    fn ambiguous_column_postgres() {
        let db = users_db();
        let hint = hint_for_error(&db, r#"column reference "id" is ambiguous"#).expect("hint");
        assert!(matches!(hint.kind, HintKind::AmbiguousColumn));
        assert_eq!(hint.original_identifier, "id");
        assert!(hint.human_message.contains("ambiguous"));
    }
}
