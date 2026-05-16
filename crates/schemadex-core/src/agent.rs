//! Agent-facing describe API. Takes a [`Database`] + a token budget and
//! produces a compact, ranked schema description suitable for an LLM prompt.

use crate::error::{Result, SchemadexError};
use crate::examples::generate_examples;
use crate::model::{Database, Table};

#[derive(Debug, Clone)]
pub struct DescribeOptions {
    pub max_tokens: usize,
    pub hint: Option<String>,
    pub tables: Option<Vec<String>>,
    pub include_samples: bool,
    pub include_foreign_keys: bool,
    /// When true, append a short list of generated few-shot SELECT examples
    /// after each table's columns + FKs. Off by default — examples cost
    /// tokens and most agents do fine without them.
    pub include_examples: bool,
}

impl Default for DescribeOptions {
    fn default() -> Self {
        Self {
            max_tokens: 2048,
            hint: None,
            tables: None,
            include_samples: true,
            include_foreign_keys: true,
            include_examples: false,
        }
    }
}

/// Render a token-budgeted description of a schema. Truncation hierarchy:
/// 1. Drop unrelated tables (low relevance to `hint`)
/// 2. Drop sample values (`[n=…]`, `[sentinel: …]`, `[top: …]`)
/// 3. Drop few-shot examples (only present when `include_examples`)
/// 4. Drop column comments (and the `-- CHECK …` annotation that rides them)
/// 5. Drop foreign keys
/// 6. Drop columns beyond ordinal 8 per table
///
/// `UNIQUE` and `GENERATED AS (…)` annotations live on the column type line
/// itself and are not dropped — they're cheap and load-bearing.
///
/// Returns a string and the estimated token count.
pub fn describe_for_agent(db: &Database, opts: &DescribeOptions) -> Result<(String, usize)> {
    let tokenizer = tiktoken_rs::cl100k_base()
        .map_err(|e| SchemadexError::Other(format!("tiktoken init failed: {e}")))?;

    let mut tables: Vec<&Table> = if let Some(ref names) = opts.tables {
        names.iter().filter_map(|n| db.table(n)).collect()
    } else {
        db.tables.iter().collect()
    };

    if let Some(ref hint) = opts.hint {
        let h = hint.to_lowercase();

        // First pass: score every table by hint overlap.
        let mut scores: Vec<u32> = tables.iter().map(|t| relevance(t, &h)).collect();

        // Second pass: identify top-scoring tables (positive scores only) and
        // boost any other table that shares a foreign key with them. The
        // boost rewards joined-along-FK companions even when their own
        // name/columns don't overlap the hint.
        let top_score = scores.iter().copied().max().unwrap_or(0);
        if top_score > 0 {
            let top_names: Vec<String> = tables
                .iter()
                .zip(scores.iter())
                .filter(|(_, s)| **s == top_score)
                .flat_map(|(t, _)| {
                    // Match on both bare and qualified names so FK lookups
                    // hit regardless of how `referenced_table` is spelled.
                    vec![
                        t.name.to_lowercase(),
                        t.qualified_name().to_lowercase(),
                    ]
                })
                .collect();

            for (idx, t) in tables.iter().enumerate() {
                if scores[idx] == top_score {
                    continue;
                }
                // Outgoing FK: this table references a top-scoring table.
                let outgoing = t
                    .foreign_keys
                    .iter()
                    .any(|fk| top_names.contains(&fk.referenced_table.to_lowercase()));
                // Incoming FK: some top-scoring table references this one.
                let self_bare = t.name.to_lowercase();
                let self_qual = t.qualified_name().to_lowercase();
                let incoming = tables.iter().zip(scores.iter()).any(|(other, s)| {
                    *s == top_score
                        && other.foreign_keys.iter().any(|fk| {
                            let rt = fk.referenced_table.to_lowercase();
                            rt == self_bare || rt == self_qual
                        })
                });
                if outgoing || incoming {
                    scores[idx] = scores[idx].saturating_add(5);
                }
            }
        }

        // Sort tables by the (boosted) score, descending.
        let mut indexed: Vec<(usize, &Table)> = tables.iter().copied().enumerate().collect();
        indexed.sort_by_key(|(i, _)| std::cmp::Reverse(scores[*i]));
        tables = indexed.into_iter().map(|(_, t)| t).collect();
    }

    let mut levels = [true, true, true, true, true];
    loop {
        let rendered = render(&tables, opts, &levels);
        let tokens = tokenizer.encode_with_special_tokens(&rendered).len();
        if tokens <= opts.max_tokens {
            return Ok((rendered, tokens));
        }
        if !drop_one_level(&mut levels, &mut tables) {
            return Err(SchemadexError::TokenBudget {
                needed: tokens,
                budget: opts.max_tokens,
            });
        }
    }
}

/// Maximum number of generated SELECT examples to attach per table when
/// `DescribeOptions::include_examples` is enabled.
const MAX_EXAMPLES_PER_TABLE: usize = 4;

fn relevance(t: &Table, hint: &str) -> u32 {
    let name = t.qualified_name().to_lowercase();
    let mut score = 0u32;
    for tok in hint.split_whitespace() {
        if name.contains(tok) {
            score += 10;
        }
        for c in &t.columns {
            if c.name.to_lowercase().contains(tok) {
                score += 1;
            }
        }
    }
    score
}

/// Truncation order: samples first (cheapest signal), then examples,
/// comments, FKs, then long-tail columns.
///
/// `levels[0]` = include samples (cardinality / top values / sentinels)
/// `levels[1]` = include few-shot examples
/// `levels[2]` = include comments (incl. `-- CHECK …`)
/// `levels[3]` = include FKs
/// `levels[4]` = include columns past ordinal 8
fn drop_one_level(levels: &mut [bool; 5], tables: &mut Vec<&Table>) -> bool {
    for slot in levels.iter_mut() {
        if *slot {
            *slot = false;
            return true;
        }
    }
    if tables.len() > 1 {
        tables.pop();
        return true;
    }
    false
}

fn render(tables: &[&Table], opts: &DescribeOptions, levels: &[bool; 5]) -> String {
    let include_samples = levels[0] && opts.include_samples;
    let include_examples = levels[1] && opts.include_examples;
    let include_comments = levels[2];
    let include_fks = levels[3] && opts.include_foreign_keys;
    let include_extra_cols = levels[4];

    let mut out = String::new();
    for t in tables {
        out.push_str(&format!("# {}\n", t.qualified_name()));
        if include_comments {
            if let Some(c) = &t.comment {
                out.push_str(&format!("> {c}\n"));
            }
        }
        let limit = if include_extra_cols {
            t.columns.len()
        } else {
            t.columns.len().min(8)
        };
        for col in t.columns.iter().take(limit) {
            let null = if col.nullable { "" } else { " NOT NULL" };
            out.push_str(&format!("- {}: {}{}", col.name, col.native_type, null));
            // UNIQUE / GENERATED annotations are cheap and load-bearing: a
            // single token each, never dropped by the truncation hierarchy.
            if col.is_unique {
                out.push_str(" UNIQUE");
            }
            if let Some(expr) = &col.generation_expression {
                out.push_str(&format!(" GENERATED AS ({expr})"));
            }
            if include_samples {
                if let Some(sample) = &col.sample {
                    if let Some(n) = sample.stats.distinct_count {
                        out.push_str(&format!(" [n={n}]"));
                    }
                }
            }
            if include_comments {
                let mut trailing = String::new();
                if let Some(c) = &col.comment {
                    trailing.push_str(c);
                }
                if let Some(check) = &col.check_constraint {
                    if !trailing.is_empty() {
                        trailing.push_str("; ");
                    }
                    trailing.push_str("CHECK ");
                    trailing.push_str(check);
                }
                if !trailing.is_empty() {
                    out.push_str(&format!(" -- {trailing}"));
                }
            }
            if include_samples {
                if let Some(sample) = &col.sample {
                    if let Some((val, frac)) = &sample.sentinel {
                        out.push_str(&format!(" [sentinel: {}={:.0}%]", val, frac * 100.0));
                    } else if !sample.top_values.is_empty() {
                        let preview = sample
                            .top_values
                            .iter()
                            .take(3)
                            .map(|(v, f)| format!("{}({:.0}%)", v, f * 100.0))
                            .collect::<Vec<_>>()
                            .join(", ");
                        out.push_str(&format!(" [top: {preview}]"));
                    }
                }
            }
            out.push('\n');
        }
        if include_fks {
            for fk in &t.foreign_keys {
                out.push_str(&format!(
                    "  FK {} -> {}({})\n",
                    fk.columns.join(","),
                    fk.referenced_table,
                    fk.referenced_columns.join(",")
                ));
            }
        }
        if include_examples {
            let examples = generate_examples(t, MAX_EXAMPLES_PER_TABLE);
            if !examples.is_empty() {
                out.push_str("Examples:\n");
                for ex in &examples {
                    out.push_str(&format!("- {ex}\n"));
                }
            }
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Column, DataType, ForeignKey, Table};

    fn small_db() -> Database {
        Database {
            backend: "test".to_string(),
            url_hash: "x".to_string(),
            fingerprint: None,
            tables: vec![Table {
                schema: None,
                name: "users".to_string(),
                comment: None,
                columns: vec![Column {
                    name: "id".to_string(),
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
                }],
                primary_key: None,
                foreign_keys: vec![],
                row_count_estimate: None,
                ddl_hash: None,
            }],
        }
    }

    #[test]
    fn fits_in_budget() {
        let db = small_db();
        let (text, tokens) = describe_for_agent(
            &db,
            &DescribeOptions {
                max_tokens: 1000,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(text.contains("users"));
        assert!(tokens < 1000);
    }

    #[test]
    fn fk_boost_lifts_related_table() {
        // `customers` has no hint-overlap with "orders", but `orders` has
        // an FK pointing at it. The boost should lift `customers` above
        // other unrelated tables while keeping `orders` (the direct match)
        // on top.
        let db = Database {
            backend: "test".to_string(),
            url_hash: "x".to_string(),
            fingerprint: None,
            tables: vec![
                Table {
                    schema: None,
                    name: "orders".to_string(),
                    comment: None,
                    columns: vec![
                        Column {
                            name: "id".to_string(),
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
                        },
                        Column {
                            name: "customer_id".to_string(),
                            data_type: DataType::Integer,
                            native_type: "int".to_string(),
                            nullable: false,
                            default: None,
                            comment: None,
                            ordinal: 1,
                            sample: None,
                            check_constraint: None,
                            is_unique: false,
                            generation_expression: None,
                        },
                    ],
                    primary_key: None,
                    foreign_keys: vec![ForeignKey {
                        name: None,
                        columns: vec!["customer_id".to_string()],
                        referenced_table: "customers".to_string(),
                        referenced_columns: vec!["id".to_string()],
                    }],
                    row_count_estimate: None,
                    ddl_hash: None,
                },
                Table {
                    schema: None,
                    name: "customers".to_string(),
                    comment: None,
                    columns: vec![Column {
                        name: "id".to_string(),
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
                    }],
                    primary_key: None,
                    foreign_keys: vec![],
                    row_count_estimate: None,
                    ddl_hash: None,
                },
            ],
        };

        let (text, _tokens) = describe_for_agent(
            &db,
            &DescribeOptions {
                max_tokens: 200,
                hint: Some("orders".to_string()),
                tables: None,
                ..Default::default()
            },
        )
        .unwrap();

        let orders_pos = text.find("# orders").expect("orders rendered");
        let customers_pos = text.find("# customers").expect("customers rendered");
        assert!(
            orders_pos < customers_pos,
            "orders should appear before customers due to FK companion boost:\n{text}"
        );
    }

    #[test]
    fn examples_render_when_enabled() {
        // Sanity check: with include_examples=true we get the "Examples:"
        // block and at least one generated SELECT for the single table.
        // With it off (the default), the block must be absent.
        let db = small_db();
        let (with_examples, _) = describe_for_agent(
            &db,
            &DescribeOptions {
                max_tokens: 1000,
                include_examples: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(
            with_examples.contains("Examples:"),
            "examples block missing:\n{with_examples}"
        );
        assert!(
            with_examples.contains("SELECT id FROM users"),
            "expected PK scan in examples:\n{with_examples}"
        );

        let (without, _) = describe_for_agent(
            &db,
            &DescribeOptions {
                max_tokens: 1000,
                include_examples: false,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(
            !without.contains("Examples:"),
            "examples should be gated off by default:\n{without}"
        );
    }

    #[test]
    fn unique_and_check_render() {
        // UNIQUE annotation lives on the type line; CHECK rides the comment
        // pass and only renders when comments are on (default).
        let mut db = small_db();
        db.tables[0].columns[0].is_unique = true;
        db.tables[0].columns[0].check_constraint = Some("id > 0".to_string());
        let (text, _) = describe_for_agent(
            &db,
            &DescribeOptions {
                max_tokens: 1000,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(text.contains("UNIQUE"), "UNIQUE annotation missing:\n{text}");
        assert!(
            text.contains("CHECK id > 0"),
            "CHECK annotation missing:\n{text}"
        );
    }
}
