//! Generate a handful of valid SELECT statements per table for the
//! agent to read as few-shot examples. Built from the cached schema
//! plus any sampled values we have on disk.
//!
//! The intent is "here's what a query that exercises this table
//! actually looks like", not "here's the answer to a particular
//! user question."

use crate::model::{Database, Table};

/// Generate up to `max_examples` SELECT statements that exercise `table`.
/// Examples are constructed from cached metadata only — no DB round-trip.
///
/// The shapes emitted, in priority order:
/// 1. A LIMIT scan of the primary-key column (or first column).
/// 2. A projection of the first 2–3 columns.
/// 3. A `WHERE col != 'sentinel'` query whenever a column has a sentinel
///    sample value — the "exclude the dominant value" pattern.
/// 4. A `JOIN` query along the first foreign key, if any.
pub fn generate_examples(table: &Table, max_examples: usize) -> Vec<String> {
    if max_examples == 0 {
        return Vec::new();
    }
    let qn = table.qualified_name();
    let pk_col = table
        .primary_key
        .as_ref()
        .and_then(|pk| pk.columns.first().cloned())
        .or_else(|| table.columns.first().map(|c| c.name.clone()));

    let mut out: Vec<String> = Vec::new();

    // 1) simple LIMIT scan
    if let Some(pk) = &pk_col {
        out.push(format!("SELECT {pk} FROM {qn} LIMIT 10"));
    } else {
        out.push(format!("SELECT * FROM {qn} LIMIT 10"));
    }

    // 2) projection of the first 3 columns
    let proj: Vec<String> = table
        .columns
        .iter()
        .take(3)
        .map(|c| c.name.clone())
        .collect();
    if proj.len() >= 2 {
        out.push(format!("SELECT {} FROM {qn} LIMIT 10", proj.join(", ")));
    }

    // 3) sample-driven WHERE: if any column has a sentinel, write a
    //    "exclude the dominant value" query — exactly the Nokia case.
    for col in &table.columns {
        if let Some(sample) = &col.sample {
            if let Some((val, _)) = &sample.sentinel {
                out.push(format!(
                    "SELECT count(*) FROM {qn} WHERE {} != '{}'",
                    col.name,
                    val.replace('\'', "''")
                ));
                break;
            }
        }
    }

    // 4) FK join, if any
    if let Some(fk) = table.foreign_keys.first() {
        if let (Some(c0), Some(rc0)) = (fk.columns.first(), fk.referenced_columns.first()) {
            let other = &fk.referenced_table;
            let pk = pk_col.as_deref().unwrap_or(c0);
            out.push(format!(
                "SELECT a.{pk}, b.{rc0} FROM {qn} a JOIN {other} b ON a.{c0} = b.{rc0} LIMIT 10"
            ));
        }
    }

    out.truncate(max_examples);
    out
}

/// Generate examples for every table in `db`, returning `(qualified_name, examples)`
/// tuples. Useful for serving a whole-schema few-shot dump.
pub fn generate_examples_for_database(
    db: &Database,
    per_table: usize,
) -> Vec<(String, Vec<String>)> {
    db.tables
        .iter()
        .map(|t| (t.qualified_name(), generate_examples(t, per_table)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        Column, ColumnSample, DataType, ForeignKey, PrimaryKey, SampleStats, Table,
    };

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
            columns: vec![col("id"), col("customer_id"), col("delay_code")],
            primary_key: Some(PrimaryKey {
                name: None,
                columns: vec!["id".to_string()],
            }),
            foreign_keys: vec![],
            row_count_estimate: None,
            ddl_hash: None,
        }
    }

    #[test]
    fn simple_pk_scan_emitted() {
        let t = orders_table();
        let out = generate_examples(&t, 4);
        assert!(
            out.iter().any(|s| s == "SELECT id FROM orders LIMIT 10"),
            "expected PK scan, got {out:?}"
        );
    }

    #[test]
    fn sentinel_drives_neq_query() {
        let mut t = orders_table();
        // delay_code is dominated by "No Delay" 80% of the time.
        t.columns[2].sample = Some(ColumnSample {
            stats: SampleStats::default(),
            top_values: vec![("No Delay".to_string(), 0.8)],
            sentinel: Some(("No Delay".to_string(), 0.8)),
        });
        let out = generate_examples(&t, 4);
        assert!(
            out.iter()
                .any(|s| s.contains("WHERE delay_code != 'No Delay'")),
            "expected sentinel-driven NEQ, got {out:?}"
        );
    }

    #[test]
    fn fk_join_when_fk_present() {
        let mut t = orders_table();
        t.foreign_keys.push(ForeignKey {
            name: None,
            columns: vec!["customer_id".to_string()],
            referenced_table: "customers".to_string(),
            referenced_columns: vec!["id".to_string()],
        });
        let out = generate_examples(&t, 4);
        assert!(
            out.iter().any(|s| s.contains("JOIN customers")),
            "expected FK join, got {out:?}"
        );
    }

    #[test]
    fn no_examples_when_max_zero() {
        let t = orders_table();
        let out = generate_examples(&t, 0);
        assert!(out.is_empty());
    }

    #[test]
    fn sentinel_apostrophe_escaped() {
        // A sentinel value containing a single quote must round-trip into a
        // syntactically valid SQL literal.
        let mut t = orders_table();
        t.columns[2].sample = Some(ColumnSample {
            stats: SampleStats::default(),
            top_values: vec![],
            sentinel: Some(("O'Brien".to_string(), 0.7)),
        });
        let out = generate_examples(&t, 4);
        assert!(
            out.iter().any(|s| s.contains("'O''Brien'")),
            "expected escaped apostrophe, got {out:?}"
        );
    }

    #[test]
    fn database_wide_helper_covers_every_table() {
        let db = Database {
            backend: "test".to_string(),
            url_hash: "x".to_string(),
            fingerprint: None,
            tables: vec![orders_table()],
        };
        let out = generate_examples_for_database(&db, 2);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].0, "orders");
        assert!(out[0].1.len() <= 2);
    }
}
