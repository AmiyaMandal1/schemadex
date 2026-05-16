//! Heuristic, regex-based SQL pre-validation against a cached [`Database`].
//!
//! This is intentionally not a real SQL parser. It extracts table and column
//! identifiers using cheap pattern matching and checks each against the cached
//! schema. The intent is to catch the most common agent errors — typo'd
//! column names, plural-vs-singular table names — before the query ever hits
//! the database. A clean (empty) result means "probably safe"; a non-empty
//! result means at least one referenced identifier is unknown and the agent
//! should fix it before retrying.
//!
//! Edge cases the heuristic deliberately ignores:
//!
//! - CTEs that shadow real table names
//! - Subqueries with their own FROM clauses
//! - Aliases of aliases
//! - String literals or column comments that contain SQL keywords
//! - Quoted identifiers with embedded `.` or whitespace
//!
//! When in doubt the validator stays silent rather than emit a false positive.

use crate::model::{Database, Table};
use crate::resolve::resolve_column;
use regex::Regex;
use serde::Serialize;
use std::collections::HashSet;
use std::sync::OnceLock;

/// A single problem found while pre-validating a SQL query against the cache.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ValidationIssue {
    pub kind: IssueKind,
    pub identifier: String,
    pub suggestion: Option<String>,
    pub confidence: Option<f32>,
}

/// What kind of identifier the validator couldn't match against the cache.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum IssueKind {
    /// A table named in `FROM` / `JOIN` does not exist in the cached schema.
    UnknownTable,
    /// A column reference does not exist on its (attributed) table.
    UnknownColumn { table: String },
}

/// Pre-validate a SQL query against a cached [`Database`]. Returns the list of
/// validation issues without executing the query. An empty `Vec` means the
/// query looks safe to run; non-empty means at least one referenced table or
/// column doesn't exist in the cache.
pub fn validate_sql(db: &Database, sql: &str) -> Vec<ValidationIssue> {
    let mut issues = Vec::new();

    let normalized = strip_string_literals(sql);

    // ----- Tables -------------------------------------------------------
    let table_refs = extract_tables(&normalized);

    // Resolve each table against the cache. `unknown_tables` keeps the names
    // we couldn't match so we don't try to attribute columns to them.
    let mut unknown_tables: HashSet<String> = HashSet::new();
    for (raw_name, _alias) in &table_refs {
        if db.table(raw_name).is_none() {
            let suggestion = fuzzy_table(db, raw_name);
            unknown_tables.insert(raw_name.to_lowercase());
            let (suggested, confidence) = match suggestion {
                Some((name, score)) => (Some(name), Some(score)),
                None => (None, None),
            };
            issues.push(ValidationIssue {
                kind: IssueKind::UnknownTable,
                identifier: raw_name.clone(),
                suggestion: suggested,
                confidence,
            });
        }
    }

    // Build an alias → real-table map for the known tables only.
    let alias_map = build_alias_map(db, &table_refs);

    // ----- Columns ------------------------------------------------------
    let column_refs = extract_columns(&normalized);

    // For each (qualifier, column) pair, route through the right table.
    let known_tables: Vec<&Table> = table_refs
        .iter()
        .filter_map(|(name, _alias)| db.table(name))
        .collect();

    let single_table = if known_tables.len() == 1 {
        Some(known_tables[0])
    } else {
        None
    };

    // De-duplicate column issues so the same typo only gets reported once
    // per table.
    let mut seen: HashSet<(String, String)> = HashSet::new();

    for (qualifier, column) in column_refs {
        if is_reserved_word(&column) || is_numeric_literal(&column) {
            continue;
        }
        if let Some(qual) = qualifier {
            // Qualified reference (alias.col or table.col). If we can find a
            // real table for the qualifier, check the column against it.
            let table = alias_map
                .get(&qual.to_lowercase())
                .copied()
                .or_else(|| db.table(&qual));
            if let Some(t) = table {
                check_column_on_table(t, &column, &mut seen, &mut issues);
            }
        } else if let Some(t) = single_table {
            // Bare column with a single-table query: attribute to that table.
            check_column_on_table(t, &column, &mut seen, &mut issues);
        }
        // Multi-table case with bare column: skip — too ambiguous.
    }

    let _ = unknown_tables; // currently informational; reserved for future use
    issues
}

/// Emit an `UnknownColumn` issue if `column` is not present on `table`.
fn check_column_on_table(
    table: &Table,
    column: &str,
    seen: &mut HashSet<(String, String)>,
    issues: &mut Vec<ValidationIssue>,
) {
    if table.column(column).is_some() {
        return;
    }
    let key = (table.name.to_lowercase(), column.to_lowercase());
    if !seen.insert(key) {
        return;
    }
    let resolved = resolve_column(table, column);
    let (suggestion, confidence) = match resolved.matched {
        Some(name) if resolved.confidence >= 0.6 => (Some(name), Some(resolved.confidence)),
        _ => (None, None),
    };
    issues.push(ValidationIssue {
        kind: IssueKind::UnknownColumn {
            table: table.name.clone(),
        },
        identifier: column.to_string(),
        suggestion,
        confidence,
    });
}

/// Build a map from alias (or table name) → real `Table` for tables we
/// recognise.
fn build_alias_map<'a>(
    db: &'a Database,
    table_refs: &'a [(String, Option<String>)],
) -> std::collections::HashMap<String, &'a Table> {
    let mut map = std::collections::HashMap::new();
    for (raw, alias) in table_refs {
        let Some(t) = db.table(raw) else { continue };
        if let Some(a) = alias {
            map.insert(a.to_lowercase(), t);
        }
        map.insert(t.name.to_lowercase(), t);
        // Also accept the bare name the user wrote (in case it's a
        // schema-qualified reference we matched).
        map.insert(raw.to_lowercase(), t);
    }
    map
}

/// Top fuzzy match (by Jaro-Winkler) over the table list. Returns the best
/// candidate name and its score, if any.
fn fuzzy_table(db: &Database, candidate: &str) -> Option<(String, f32)> {
    if db.tables.is_empty() {
        return None;
    }
    let cand_norm = normalize(candidate);
    let mut scored: Vec<(&Table, f32)> = db
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
            (t, (jw + bonus).min(1.0))
        })
        .collect();
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let (best, score) = scored[0];
    if score < 0.6 {
        return None;
    }
    Some((best.name.clone(), score))
}

fn normalize(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Replace the contents of every quoted string literal with spaces so the
/// downstream regex passes don't latch onto identifiers that live inside
/// `'...'`. We leave `"..."` alone because some dialects use it for quoted
/// identifiers.
fn strip_string_literals(sql: &str) -> String {
    let mut out = String::with_capacity(sql.len());
    let bytes = sql.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\'' {
            out.push(' ');
            i += 1;
            while i < bytes.len() {
                if bytes[i] == b'\'' {
                    // Handle escaped `''`
                    if i + 1 < bytes.len() && bytes[i + 1] == b'\'' {
                        i += 2;
                        continue;
                    }
                    i += 1;
                    out.push(' ');
                    break;
                }
                i += 1;
            }
        } else {
            out.push(bytes[i] as char);
            i += 1;
        }
    }
    out
}

/// Extract `(table_name, alias)` from FROM / JOIN clauses. Returns a Vec —
/// duplicates are allowed and preserved (each entry maps to one usage).
fn extract_tables(sql: &str) -> Vec<(String, Option<String>)> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(
            r#"(?ix)
                \b (?: from | join ) \b
                \s+
                # captured group 1: table name, optionally quoted/backticked
                (?:
                    " (?P<dq> [^"]+ ) "
                  | ` (?P<bt> [^`]+ ) `
                  | (?P<plain> [A-Za-z_][A-Za-z0-9_.]* )
                )
                # optional alias: `AS foo` or bare `foo`
                (?:
                    \s+ (?: as \s+ )?
                    (?:
                        " (?P<adq> [^"]+ ) "
                      | (?P<aplain> [A-Za-z_][A-Za-z0-9_]* )
                    )
                )?
            "#,
        )
        .expect("table regex")
    });

    let mut out = Vec::new();
    for caps in re.captures_iter(sql) {
        let name = caps
            .name("dq")
            .or_else(|| caps.name("bt"))
            .or_else(|| caps.name("plain"))
            .map(|m| m.as_str().to_string());
        let Some(name) = name else { continue };
        // Reject `from` keywords like `FROM (subquery)` — those have no name.
        if name.is_empty() {
            continue;
        }
        let alias = caps
            .name("adq")
            .or_else(|| caps.name("aplain"))
            .map(|m| m.as_str().to_string())
            .filter(|s| !is_reserved_word(s));
        out.push((name, alias));
    }
    out
}

/// Extract column references. Returns `(qualifier, column)` pairs where
/// `qualifier` is the alias / table prefix (`u.id` → `("u", "id")`) or
/// `None` for bare references.
///
/// Sources:
///
/// 1. The SELECT projection list (between `SELECT` and the next `FROM`).
///    Split on top-level commas, take the left side of any `AS`.
/// 2. Anywhere a `qualifier.identifier` or bare identifier appears followed
///    by a comparison / list operator (`=`, `<`, `>`, `!`, `,`).
fn extract_columns(sql: &str) -> Vec<(Option<String>, String)> {
    let mut out = Vec::new();

    // 1. SELECT projection list
    if let Some((projection, _rest)) = extract_select_projection(sql) {
        for raw_expr in split_top_level_commas(&projection) {
            let expr = strip_alias(raw_expr.trim());
            if expr == "*" || expr.is_empty() {
                continue;
            }
            if let Some(idref) = parse_simple_identifier(expr) {
                out.push(idref);
            }
        }
    }

    // 2. Identifier-before-operator pattern. Match `(qualifier.)?ident` that
    //    is immediately followed by whitespace and a comparison or comma.
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(
            r#"(?x)
                (?: (?P<qual> [A-Za-z_][A-Za-z0-9_]* ) \. )?
                (?P<col> [A-Za-z_][A-Za-z0-9_]* )
                \s* (?: = | < | > | ! | \,)
            "#,
        )
        .expect("column op regex")
    });
    for caps in re.captures_iter(sql) {
        let qual = caps.name("qual").map(|m| m.as_str().to_string());
        let col = caps["col"].to_string();
        out.push((qual, col));
    }

    out
}

/// Slice out the projection list — the text strictly between the first
/// top-level `SELECT` and the next top-level `FROM`. Returns `None` if no
/// matching pair is found.
fn extract_select_projection(sql: &str) -> Option<(String, &str)> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r#"(?is)\bselect\b(.*?)\bfrom\b"#).expect("select regex")
    });
    let caps = re.captures(sql)?;
    let projection = caps.get(1)?.as_str().to_string();
    Some((projection, ""))
}

/// Split on top-level `,` (not inside parens).
fn split_top_level_commas(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth: i32 = 0;
    let mut start = 0usize;
    for (i, c) in s.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth = (depth - 1).max(0),
            ',' if depth == 0 => {
                out.push(s[start..i].to_string());
                start = i + 1;
            }
            _ => {}
        }
    }
    if start < s.len() {
        out.push(s[start..].to_string());
    }
    out
}

/// Drop a trailing `AS alias` or bare alias from `expr` and return whatever's
/// on the left. Keeps things like `count(*)` intact.
fn strip_alias(expr: &str) -> &str {
    // Find `AS` not inside parens.
    let bytes = expr.as_bytes();
    let lower = expr.to_ascii_lowercase();
    let lbytes = lower.as_bytes();
    let mut depth: i32 = 0;
    let mut i = 0;
    while i + 3 < bytes.len() {
        match bytes[i] {
            b'(' => depth += 1,
            b')' => depth = (depth - 1).max(0),
            b' ' | b'\t' | b'\n' => {
                if depth == 0 && i + 3 < lbytes.len() {
                    // Look for whitespace + "as" + whitespace
                    if &lbytes[i + 1..i + 3] == b"as"
                        && i + 3 < bytes.len()
                        && bytes[i + 3].is_ascii_whitespace()
                    {
                        return expr[..i].trim();
                    }
                }
            }
            _ => {}
        }
        i += 1;
    }
    expr.trim()
}

/// Parse `expr` as a (possibly qualified) bare identifier. Returns `None`
/// for function calls, arithmetic expressions, literals, etc.
fn parse_simple_identifier(expr: &str) -> Option<(Option<String>, String)> {
    let expr = expr.trim();
    // Reject anything with whitespace or non-identifier characters except `.`
    let ok = expr
        .chars()
        .all(|c| c.is_alphanumeric() || c == '_' || c == '.');
    if !ok || expr.is_empty() {
        return None;
    }
    let parts: Vec<&str> = expr.split('.').collect();
    match parts.as_slice() {
        [col] => {
            if is_reserved_word(col) || is_numeric_literal(col) {
                return None;
            }
            Some((None, (*col).to_string()))
        }
        [qual, col] => {
            if is_reserved_word(col) || is_numeric_literal(col) {
                return None;
            }
            Some((Some((*qual).to_string()), (*col).to_string()))
        }
        // schema.table.col — treat the middle as the qualifier (the table).
        [_schema, qual, col] => {
            if is_reserved_word(col) || is_numeric_literal(col) {
                return None;
            }
            Some((Some((*qual).to_string()), (*col).to_string()))
        }
        _ => None,
    }
}

fn is_numeric_literal(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_digit() || c == '.')
}

/// Reserved-word filter. Conservative — we'd rather skip a real column than
/// flag a false positive.
fn is_reserved_word(s: &str) -> bool {
    matches!(
        s.to_ascii_lowercase().as_str(),
        "select"
            | "from"
            | "where"
            | "and"
            | "or"
            | "not"
            | "in"
            | "is"
            | "null"
            | "as"
            | "on"
            | "join"
            | "inner"
            | "outer"
            | "left"
            | "right"
            | "full"
            | "cross"
            | "lateral"
            | "using"
            | "group"
            | "by"
            | "order"
            | "having"
            | "limit"
            | "offset"
            | "distinct"
            | "all"
            | "asc"
            | "desc"
            | "with"
            | "case"
            | "when"
            | "then"
            | "else"
            | "end"
            | "between"
            | "like"
            | "ilike"
            | "exists"
            | "union"
            | "intersect"
            | "except"
            | "true"
            | "false"
            | "count"
            | "sum"
            | "avg"
            | "min"
            | "max"
    )
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

    fn users_table() -> Table {
        Table {
            schema: None,
            name: "users".to_string(),
            comment: None,
            columns: vec![col("id"), col("email")],
            primary_key: None,
            foreign_keys: vec![],
            row_count_estimate: None,
            ddl_hash: None,
        }
    }

    fn orders_table() -> Table {
        Table {
            schema: None,
            name: "orders".to_string(),
            comment: None,
            columns: vec![col("id"), col("user_id"), col("total")],
            primary_key: None,
            foreign_keys: vec![],
            row_count_estimate: None,
            ddl_hash: None,
        }
    }

    fn one_table_db() -> Database {
        Database {
            backend: "test".to_string(),
            url_hash: "x".to_string(),
            tables: vec![users_table()],
            fingerprint: None,
        }
    }

    fn two_table_db() -> Database {
        Database {
            backend: "test".to_string(),
            url_hash: "x".to_string(),
            tables: vec![users_table(), orders_table()],
            fingerprint: None,
        }
    }

    #[test]
    fn valid_select_passes() {
        let db = one_table_db();
        let issues = validate_sql(&db, "SELECT id FROM users");
        assert!(issues.is_empty(), "expected no issues, got: {:?}", issues);
    }

    #[test]
    fn unknown_table_flagged() {
        let db = one_table_db();
        let issues = validate_sql(&db, "SELECT * FROM userz");
        let tbl_issues: Vec<_> = issues
            .iter()
            .filter(|i| matches!(i.kind, IssueKind::UnknownTable))
            .collect();
        assert_eq!(tbl_issues.len(), 1, "issues: {:?}", issues);
        assert_eq!(tbl_issues[0].identifier, "userz");
        assert_eq!(tbl_issues[0].suggestion.as_deref(), Some("users"));
        assert!(tbl_issues[0].confidence.unwrap_or(0.0) > 0.6);
    }

    #[test]
    fn unknown_column_flagged() {
        let db = one_table_db();
        let issues = validate_sql(&db, "SELECT emial FROM users");
        let col_issues: Vec<_> = issues
            .iter()
            .filter(|i| matches!(i.kind, IssueKind::UnknownColumn { .. }))
            .collect();
        assert_eq!(col_issues.len(), 1, "issues: {:?}", issues);
        assert_eq!(col_issues[0].identifier, "emial");
        assert_eq!(col_issues[0].suggestion.as_deref(), Some("email"));
        match &col_issues[0].kind {
            IssueKind::UnknownColumn { table } => assert_eq!(table, "users"),
            _ => unreachable!(),
        }
    }

    #[test]
    fn join_lookup() {
        let db = two_table_db();
        let sql = "SELECT * FROM users u JOIN orderz o ON u.id = o.user_id";
        let issues = validate_sql(&db, sql);
        let tbl_issues: Vec<_> = issues
            .iter()
            .filter(|i| matches!(i.kind, IssueKind::UnknownTable))
            .collect();
        assert_eq!(tbl_issues.len(), 1, "issues: {:?}", issues);
        assert_eq!(tbl_issues[0].identifier, "orderz");
        assert_eq!(tbl_issues[0].suggestion.as_deref(), Some("orders"));
    }

    #[test]
    fn qualified_column_known_table() {
        let db = two_table_db();
        let sql = "SELECT u.emial FROM users u";
        let issues = validate_sql(&db, sql);
        let col_issues: Vec<_> = issues
            .iter()
            .filter(|i| matches!(i.kind, IssueKind::UnknownColumn { .. }))
            .collect();
        assert!(!col_issues.is_empty(), "expected column issue, got: {:?}", issues);
        assert_eq!(col_issues[0].identifier, "emial");
        assert_eq!(col_issues[0].suggestion.as_deref(), Some("email"));
    }

    #[test]
    fn string_literal_not_misread_as_identifier() {
        let db = one_table_db();
        // The literal contains `notacolumn` but it's inside quotes — must be
        // ignored.
        let issues = validate_sql(&db, "SELECT email FROM users WHERE email = 'notacolumn'");
        let col_issues: Vec<_> = issues
            .iter()
            .filter(|i| matches!(i.kind, IssueKind::UnknownColumn { .. }))
            .collect();
        assert!(col_issues.is_empty(), "got: {:?}", issues);
    }
}
