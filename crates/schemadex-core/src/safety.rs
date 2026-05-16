//! SQL safety filters. Currently: reject non-readonly statements before
//! they hit the database.

use crate::error::{Result, SchemadexError};

/// Statements we consider read-only and therefore safe to execute through
/// the agent surface. Stored uppercase for case-insensitive matching.
const READONLY_KEYWORDS: &[&str] = &["SELECT", "WITH", "EXPLAIN", "SHOW", "DESCRIBE", "DESC"];

/// Accept `SELECT`, `WITH`, and `EXPLAIN` (plus `SHOW` / `DESCRIBE` / `DESC`)
/// only. Everything else (INSERT, UPDATE, DELETE, DROP, CREATE, GRANT, ...)
/// gets a hard error.
///
/// This is a lightweight check — strip leading whitespace, comments, and
/// `EXPLAIN` / `EXPLAIN ANALYZE` prefixes, then look at the first keyword.
///
/// Multiple semicolon-separated statements are each validated; if any one is
/// not read-only the whole batch is rejected.
pub fn assert_readonly(sql: &str) -> Result<()> {
    for stmt in split_statements(sql) {
        let trimmed = stmt.trim();
        if trimmed.is_empty() {
            continue;
        }
        check_single(trimmed)?;
    }
    Ok(())
}

/// Validate a single statement (no top-level `;` splits).
fn check_single(sql: &str) -> Result<()> {
    let stripped = strip_leading_noise(sql);
    let (keyword, rest) = first_keyword(stripped);
    if keyword.is_empty() {
        return Err(reject(sql));
    }
    let upper = keyword.to_ascii_uppercase();
    if !READONLY_KEYWORDS.iter().any(|k| *k == upper) {
        return Err(reject(sql));
    }
    // For EXPLAIN, recurse into the inner statement so `EXPLAIN INSERT...`
    // (and `EXPLAIN ANALYZE INSERT...`) are also rejected.
    if upper == "EXPLAIN" {
        let inner = rest.trim_start();
        // Skip optional `ANALYZE` / `VERBOSE` etc. modifiers — they don't
        // change whether the inner statement is read-only.
        let inner = skip_explain_modifiers(inner);
        if !inner.is_empty() {
            check_single(inner)?;
        }
    }
    Ok(())
}

/// Build the rejection error with an 80-char preview of the offending SQL.
fn reject(sql: &str) -> SchemadexError {
    let preview: String = sql.chars().take(80).collect();
    SchemadexError::Other(format!(
        "read-only mode rejected non-SELECT statement: \"{preview}\""
    ))
}

/// Drop leading whitespace and `--` / `/* */` comments from the front of `sql`.
fn strip_leading_noise(mut sql: &str) -> &str {
    loop {
        let before = sql;
        sql = sql.trim_start();
        if let Some(rest) = sql.strip_prefix("--") {
            // Line comment: chew until newline.
            match rest.find('\n') {
                Some(idx) => sql = &rest[idx + 1..],
                None => sql = "",
            }
        } else if let Some(rest) = sql.strip_prefix("/*") {
            // Block comment: chew until `*/`.
            match rest.find("*/") {
                Some(idx) => sql = &rest[idx + 2..],
                None => sql = "",
            }
        }
        if sql.len() == before.len() {
            // Nothing was stripped this round — we're done.
            break;
        }
    }
    sql
}

/// Pull the first ASCII-alphabetic run off the front of `sql` and return
/// `(keyword, remainder)`. Non-alphabetic leading chars yield an empty
/// keyword and the original string.
fn first_keyword(sql: &str) -> (&str, &str) {
    let bytes = sql.as_bytes();
    let mut i = 0;
    while i < bytes.len() && bytes[i].is_ascii_alphabetic() {
        i += 1;
    }
    (&sql[..i], &sql[i..])
}

/// Strip optional EXPLAIN modifiers like `ANALYZE`, `VERBOSE`,
/// `(ANALYZE, VERBOSE)` so the next `first_keyword` lands on the real
/// inner statement keyword.
fn skip_explain_modifiers(sql: &str) -> &str {
    let mut s = sql.trim_start();
    // Parenthesised list: `EXPLAIN (ANALYZE, VERBOSE) SELECT ...`
    if let Some(rest) = s.strip_prefix('(') {
        match rest.find(')') {
            Some(idx) => s = rest[idx + 1..].trim_start(),
            None => return "",
        }
    }
    // Bare modifier words. Loop in case multiple stack
    // (e.g. `EXPLAIN ANALYZE VERBOSE SELECT ...`).
    loop {
        let (kw, rest) = first_keyword(s);
        if kw.is_empty() {
            break;
        }
        let upper = kw.to_ascii_uppercase();
        if matches!(upper.as_str(), "ANALYZE" | "VERBOSE" | "FORMAT") {
            s = rest.trim_start();
            // `FORMAT JSON` etc. — consume the value too.
            if upper == "FORMAT" {
                let (_, r) = first_keyword(s);
                s = r.trim_start();
            }
            continue;
        }
        break;
    }
    s
}

/// Split `sql` on top-level `;` boundaries (i.e. not inside `'…'` or `"…"`
/// string/identifier literals). Best-effort — we are not a real SQL parser,
/// but this is good enough to catch `SELECT 1; DELETE FROM t`.
fn split_statements(sql: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let bytes = sql.as_bytes();
    let mut start = 0usize;
    let mut i = 0usize;
    let mut in_single = false;
    let mut in_double = false;
    while i < bytes.len() {
        let b = bytes[i];
        match b {
            b'\'' if !in_double => {
                // Handle escaped `''` inside single-quoted literals.
                if in_single && i + 1 < bytes.len() && bytes[i + 1] == b'\'' {
                    i += 2;
                    continue;
                }
                in_single = !in_single;
            }
            b'"' if !in_single => {
                in_double = !in_double;
            }
            b';' if !in_single && !in_double => {
                out.push(&sql[start..i]);
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    if start < sql.len() {
        out.push(&sql[start..]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_select() {
        assert!(assert_readonly("SELECT 1").is_ok());
        assert!(assert_readonly("  select id from users").is_ok());
    }

    #[test]
    fn accepts_with_cte() {
        let sql = "WITH t AS (SELECT 1) SELECT * FROM t";
        assert!(assert_readonly(sql).is_ok());
    }

    #[test]
    fn accepts_explain_select() {
        assert!(assert_readonly("EXPLAIN SELECT * FROM users").is_ok());
        assert!(assert_readonly("EXPLAIN ANALYZE SELECT * FROM users").is_ok());
        assert!(assert_readonly("EXPLAIN (ANALYZE, VERBOSE) SELECT 1").is_ok());
    }

    #[test]
    fn rejects_insert() {
        let err = assert_readonly("INSERT INTO t VALUES (1)").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("read-only"), "got: {msg}");
        assert!(msg.contains("INSERT"), "got: {msg}");
    }

    #[test]
    fn rejects_drop_table() {
        let err = assert_readonly("DROP TABLE users").unwrap_err();
        assert!(err.to_string().contains("read-only"));
    }

    #[test]
    fn rejects_explain_insert() {
        let err = assert_readonly("EXPLAIN INSERT INTO t VALUES (1)").unwrap_err();
        assert!(err.to_string().contains("read-only"));
        let err = assert_readonly("EXPLAIN ANALYZE DELETE FROM t").unwrap_err();
        assert!(err.to_string().contains("read-only"));
    }

    #[test]
    fn rejects_compound_with_write() {
        let err = assert_readonly("SELECT 1; DELETE FROM t").unwrap_err();
        assert!(err.to_string().contains("read-only"));
    }

    #[test]
    fn strips_leading_comments() {
        assert!(assert_readonly("-- hello\nSELECT 1").is_ok());
        assert!(assert_readonly("/* hi */ SELECT 1").is_ok());
        assert!(assert_readonly("/* hi */ -- yo\n  SELECT 1").is_ok());
    }

    #[test]
    fn semicolon_inside_string_does_not_split() {
        // The `;` lives inside a string literal so we must NOT split here —
        // the whole thing is a single SELECT and should pass.
        assert!(assert_readonly("SELECT ';DELETE FROM t'").is_ok());
    }

    #[test]
    fn rejects_empty_or_garbage() {
        assert!(assert_readonly("123 foo").is_err());
    }
}
