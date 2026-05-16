//! Fuzzy column resolution. Given a candidate name the user/agent invented,
//! find the most likely real column on a table.

use crate::model::{Column, Table};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ResolveResult {
    pub matched: Option<String>,
    pub confidence: f32,
    pub alternatives: Vec<(String, f32)>,
}

/// Resolve a candidate column name on a table. Returns the best match plus
/// up-to-3 alternatives, each scored in `[0.0, 1.0]`.
pub fn resolve_column(table: &Table, candidate: &str) -> ResolveResult {
    if table.columns.is_empty() {
        return ResolveResult {
            matched: None,
            confidence: 0.0,
            alternatives: Vec::new(),
        };
    }

    let candidate_norm = normalize(candidate);

    let mut scored: Vec<(&Column, f32)> = table
        .columns
        .iter()
        .map(|c| {
            let name_norm = normalize(&c.name);
            (c, score(&candidate_norm, &name_norm))
        })
        .collect();

    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    let (best_col, best_score) = scored[0];
    let alternatives = scored
        .iter()
        .skip(1)
        .take(3)
        .map(|(c, s)| (c.name.clone(), *s))
        .collect();

    ResolveResult {
        matched: Some(best_col.name.clone()),
        confidence: best_score,
        alternatives,
    }
}

fn normalize(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn score(candidate: &str, name: &str) -> f32 {
    if candidate.is_empty() || name.is_empty() {
        return 0.0;
    }
    if candidate == name {
        return 1.0;
    }
    let jw = strsim::jaro_winkler(candidate, name) as f32;
    let contains_bonus = if name.contains(candidate) || candidate.contains(name) {
        0.05
    } else {
        0.0
    };
    (jw + contains_bonus).min(1.0)
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
        }
    }

    fn tbl() -> Table {
        Table {
            schema: None,
            name: "users".to_string(),
            comment: None,
            columns: vec![col("user_id"), col("created_at"), col("delay_code")],
            primary_key: None,
            foreign_keys: vec![],
            row_count_estimate: None,
            ddl_hash: None,
        }
    }

    #[test]
    fn exact_match_is_1_0() {
        let r = resolve_column(&tbl(), "user_id");
        assert_eq!(r.matched.as_deref(), Some("user_id"));
        assert!((r.confidence - 1.0).abs() < 1e-4);
    }

    #[test]
    fn typo_resolves() {
        let r = resolve_column(&tbl(), "delaycode");
        assert_eq!(r.matched.as_deref(), Some("delay_code"));
        assert!(r.confidence > 0.9);
    }
}
