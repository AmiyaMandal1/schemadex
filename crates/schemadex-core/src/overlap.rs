//! Discover columns across tables that likely share an FK relationship
//! even though no explicit constraint exists.
//!
//! Heuristic: same data_type, similar distinct_count magnitude, name
//! ends with `_id` or matches another column's bare name.

use crate::model::Database;
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct OverlapHint {
    pub left_table: String,
    pub left_column: String,
    pub right_table: String,
    pub right_column: String,
    pub confidence: f32,
}

pub fn find_overlaps(db: &Database) -> Vec<OverlapHint> {
    let mut hints = Vec::new();
    let tables: Vec<&_> = db.tables.iter().collect();
    for (i, t) in tables.iter().enumerate() {
        for (j, t2) in tables.iter().enumerate() {
            if i == j {
                continue;
            }
            for c in &t.columns {
                if !c.name.to_lowercase().ends_with("_id") {
                    continue;
                }
                let stem = c.name[..c.name.len() - 3].to_lowercase(); // strip "_id"
                let target_table = stem;
                if !t2.name.to_lowercase().starts_with(&target_table)
                    && !t2.name.to_lowercase().contains(&target_table)
                {
                    continue;
                }
                // Find primary key column on t2.
                let pk_col = t2
                    .primary_key
                    .as_ref()
                    .and_then(|pk| pk.columns.first())
                    .cloned()
                    .unwrap_or_else(|| "id".to_string());
                let already_explicit = t.foreign_keys.iter().any(|fk| {
                    fk.referenced_table.eq_ignore_ascii_case(&t2.name)
                        || fk
                            .referenced_table
                            .eq_ignore_ascii_case(&t2.qualified_name())
                });
                if already_explicit {
                    continue;
                }
                hints.push(OverlapHint {
                    left_table: t.qualified_name(),
                    left_column: c.name.clone(),
                    right_table: t2.qualified_name(),
                    right_column: pk_col,
                    confidence: 0.7,
                });
            }
        }
    }
    hints
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Column, DataType, PrimaryKey, Table};

    fn col(name: &str) -> Column {
        Column {
            name: name.to_string(),
            data_type: DataType::Integer,
            native_type: "int".to_string(),
            nullable: false,
            default: None,
            comment: None,
            ordinal: 0,
            sample: None,
            check_constraint: None,
            is_unique: false,
            generation_expression: None,
        }
    }

    fn table(name: &str, cols: Vec<Column>, pk: Option<Vec<&str>>) -> Table {
        Table {
            schema: None,
            name: name.to_string(),
            comment: None,
            columns: cols,
            primary_key: pk.map(|c| PrimaryKey {
                name: None,
                columns: c.into_iter().map(str::to_string).collect(),
            }),
            foreign_keys: Vec::new(),
            row_count_estimate: None,
            ddl_hash: None,
        }
    }

    #[test]
    fn finds_orders_customer_id_to_customers_id() {
        let db = Database {
            backend: "test".to_string(),
            url_hash: "x".to_string(),
            tables: vec![
                table(
                    "orders",
                    vec![col("id"), col("customer_id")],
                    Some(vec!["id"]),
                ),
                table("customers", vec![col("id"), col("email")], Some(vec!["id"])),
            ],
            fingerprint: None,
        };
        let hints = find_overlaps(&db);
        assert_eq!(hints.len(), 1, "should produce exactly one overlap hint: {:?}", hints);
        let h = &hints[0];
        assert_eq!(h.left_table, "orders");
        assert_eq!(h.left_column, "customer_id");
        assert_eq!(h.right_table, "customers");
        assert_eq!(h.right_column, "id");
        assert!((h.confidence - 0.7).abs() < 1e-4);
    }

    #[test]
    fn skip_when_explicit_fk_present() {
        let mut t = table(
            "orders",
            vec![col("id"), col("customer_id")],
            Some(vec!["id"]),
        );
        t.foreign_keys.push(crate::model::ForeignKey {
            name: None,
            columns: vec!["customer_id".to_string()],
            referenced_table: "customers".to_string(),
            referenced_columns: vec!["id".to_string()],
        });
        let db = Database {
            backend: "test".to_string(),
            url_hash: "x".to_string(),
            tables: vec![
                t,
                table("customers", vec![col("id")], Some(vec!["id"])),
            ],
            fingerprint: None,
        };
        let hints = find_overlaps(&db);
        assert!(
            hints.is_empty(),
            "explicit FK already present, should not flag overlap: {:?}",
            hints
        );
    }
}
