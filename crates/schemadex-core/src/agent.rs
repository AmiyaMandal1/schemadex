//! Agent-facing describe API. Takes a [`Database`] + a token budget and
//! produces a compact, ranked schema description suitable for an LLM prompt.

use crate::error::{Result, SchemadexError};
use crate::model::{Database, Table};

#[derive(Debug, Clone)]
pub struct DescribeOptions {
    pub max_tokens: usize,
    pub hint: Option<String>,
    pub tables: Option<Vec<String>>,
    pub include_samples: bool,
    pub include_foreign_keys: bool,
}

impl Default for DescribeOptions {
    fn default() -> Self {
        Self {
            max_tokens: 2048,
            hint: None,
            tables: None,
            include_samples: true,
            include_foreign_keys: true,
        }
    }
}

/// Render a token-budgeted description of a schema. Truncation hierarchy:
/// 1. Drop unrelated tables (low relevance to `hint`)
/// 2. Drop sample values
/// 3. Drop column comments
/// 4. Drop foreign keys
/// 5. Drop columns beyond ordinal 8 per table
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
        tables.sort_by_key(|t| {
            let score = relevance(t, &h);
            std::cmp::Reverse(score)
        });
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

/// `levels[0]` = include samples, `[1]` = include comments, `[2]` = include FKs,
/// `[3]` = include columns past ordinal 8, `[4]` = include type info on extras.
fn drop_one_level(levels: &mut [bool; 5], tables: &mut Vec<&Table>) -> bool {
    if levels[0] {
        levels[0] = false;
        return true;
    }
    if levels[1] {
        levels[1] = false;
        return true;
    }
    if levels[2] {
        levels[2] = false;
        return true;
    }
    if levels[3] {
        levels[3] = false;
        return true;
    }
    if tables.len() > 1 {
        tables.pop();
        return true;
    }
    false
}

fn render(tables: &[&Table], opts: &DescribeOptions, levels: &[bool; 5]) -> String {
    let include_samples = levels[0] && opts.include_samples;
    let include_comments = levels[1];
    let include_fks = levels[2] && opts.include_foreign_keys;
    let include_extra_cols = levels[3];

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
            if include_comments {
                if let Some(c) = &col.comment {
                    out.push_str(&format!(" -- {c}"));
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
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Column, DataType, Table};

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
}
