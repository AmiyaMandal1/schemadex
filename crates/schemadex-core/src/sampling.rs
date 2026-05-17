//! Sample-value collection. Backend-agnostic: each backend feeds us
//! `(value, count)` rows for categorical columns and `Vec<f64>` for numeric
//! columns, and we package them into [`crate::model::ColumnSample`].
//!
//! The 40% rule: if any single value covers more than [`SENTINEL_THRESHOLD`]
//! of the sampled rows, it's flagged. That's the `'No Delay'` case from the
//! Nokia agent story in the road map.

use crate::model::{ColumnSample, SampleStats};

pub const SENTINEL_THRESHOLD: f32 = 0.40;
pub const DEFAULT_TOP_K: usize = 10;

/// Policy describing which columns to skip sampling for on safety grounds.
///
/// Matching is case-insensitive substring containment on the lowercased
/// column name (or column comment). This is deliberately broader than an
/// exact-match list — `email`, `email_address`, and `user_email` should all
/// hit the same rule.
#[derive(Debug, Clone)]
pub struct RedactionPolicy {
    /// Skip sampling for columns whose name matches any of these
    /// case-insensitive substrings.
    pub deny_substrings: Vec<String>,
    /// Skip sampling for columns whose comment contains any of these.
    pub deny_comment_substrings: Vec<String>,
    /// Opt-in: when true, [`RedactionPolicy::should_redact_column`] also
    /// inspects the column's collected sample values via
    /// [`crate::pii::classify_column`] and returns `true` when the
    /// heuristic flags the column as containing PII. Off by default to
    /// preserve the cheap name/comment-only fast path.
    pub classify_values: bool,
}

impl RedactionPolicy {
    /// Sensible PII defaults. Errs on the side of redacting — `password`,
    /// `secret`, and `token` are catch-alls; column names like `passport`
    /// and `credit_card` cover common HR/payments cases.
    pub fn default_pii() -> Self {
        Self {
            deny_substrings: [
                "email",
                "phone",
                "ssn",
                "passport",
                "credit_card",
                "cvv",
                "password",
                "secret",
                "token",
                "api_key",
            ]
            .into_iter()
            .map(str::to_string)
            .collect(),
            deny_comment_substrings: [
                "personally_identifiable",
                "pii",
                "gdpr_sensitive",
            ]
            .into_iter()
            .map(str::to_string)
            .collect(),
            classify_values: false,
        }
    }

    /// Returns `true` when the column should be skipped by the sampler.
    pub fn should_redact(&self, column_name: &str, column_comment: Option<&str>) -> bool {
        let lname = column_name.to_lowercase();
        if self.deny_substrings.iter().any(|s| lname.contains(s)) {
            return true;
        }
        if let Some(c) = column_comment {
            let lc = c.to_lowercase();
            if self.deny_comment_substrings.iter().any(|s| lc.contains(s)) {
                return true;
            }
        }
        false
    }

    /// Like [`Self::should_redact`] but additionally consults a column's
    /// collected sample values via [`crate::pii::classify_column`] when
    /// `classify_values` is enabled. Use this when you already have a
    /// [`crate::model::Column`] in hand (e.g. post-sampling).
    pub fn should_redact_column(&self, column: &crate::model::Column) -> bool {
        if self.should_redact(&column.name, column.comment.as_deref()) {
            return true;
        }
        if self.classify_values && crate::pii::classify_column(column).is_some() {
            return true;
        }
        false
    }
}

#[derive(Debug, Clone)]
pub struct SamplingPolicy {
    pub top_k: usize,
    pub sentinel_threshold: f32,
    /// Maximum distinct values to keep before bailing out (avoid TEXT columns
    /// with 10M distinct values).
    pub max_distinct: u64,
    pub sample_rows: u64,
    /// Optional redaction policy. When `Some`, sample collection skips any
    /// column whose name or comment matches the policy.
    pub redaction: Option<RedactionPolicy>,
}

impl Default for SamplingPolicy {
    fn default() -> Self {
        Self::default_policy()
    }
}

impl SamplingPolicy {
    /// Construct the default policy. Returns a safe baseline with PII
    /// redaction enabled — callers who want raw sampling must explicitly
    /// clear `redaction` after construction.
    pub fn default_policy() -> Self {
        Self {
            top_k: DEFAULT_TOP_K,
            sentinel_threshold: SENTINEL_THRESHOLD,
            max_distinct: 1_000,
            sample_rows: 10_000,
            redaction: Some(RedactionPolicy::default_pii()),
        }
    }
}

/// Build a [`ColumnSample`] from a top-K-with-counts list. `total_non_null` is
/// the denominator used to compute fractions; pass the sampled row count
/// minus nulls.
pub fn categorical_sample(
    top: &[(String, u64)],
    total_non_null: u64,
    null_count: u64,
    distinct_count: Option<u64>,
    policy: &SamplingPolicy,
) -> ColumnSample {
    let mut top_values: Vec<(String, f32)> = top
        .iter()
        .take(policy.top_k)
        .map(|(v, c)| {
            let frac = if total_non_null == 0 {
                0.0
            } else {
                *c as f32 / total_non_null as f32
            };
            (v.clone(), frac)
        })
        .collect();
    top_values.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    let sentinel = top_values
        .first()
        .filter(|(_, f)| *f > policy.sentinel_threshold)
        .cloned();

    let total = total_non_null + null_count;
    let null_fraction = if total == 0 {
        None
    } else {
        Some(null_count as f32 / total as f32)
    };

    ColumnSample {
        stats: SampleStats {
            distinct_count,
            null_fraction,
            ..Default::default()
        },
        top_values,
        sentinel,
    }
}

pub fn numeric_sample(values: &mut [f64], null_count: u64) -> ColumnSample {
    if values.is_empty() {
        return ColumnSample {
            stats: SampleStats::default(),
            top_values: Vec::new(),
            sentinel: None,
        };
    }
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let min = values.first().copied();
    let max = values.last().copied();
    let p50 = percentile(values, 0.50);
    let p95 = percentile(values, 0.95);
    let p99 = percentile(values, 0.99);

    let total = values.len() as u64 + null_count;
    let null_fraction = if total == 0 {
        None
    } else {
        Some(null_count as f32 / total as f32)
    };

    ColumnSample {
        stats: SampleStats {
            distinct_count: None,
            null_fraction,
            min: min.map(format_float),
            max: max.map(format_float),
            p50: Some(format_float(p50)),
            p95: Some(format_float(p95)),
            p99: Some(format_float(p99)),
        },
        top_values: Vec::new(),
        sentinel: None,
    }
}

fn percentile(sorted: &[f64], q: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    let n = sorted.len();
    if q <= 0.0 {
        return sorted[0];
    }
    if q >= 1.0 {
        return sorted[n - 1];
    }
    let raw = (n as f64 * q) as isize - 1;
    let idx = raw.max(0) as usize;
    sorted[idx.min(n - 1)]
}

fn format_float(v: f64) -> String {
    if v.fract() == 0.0 {
        format!("{v:.0}")
    } else {
        format!("{v:.4}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sentinel_flags_dominant_value() {
        let policy = SamplingPolicy::default_policy();
        let s = categorical_sample(
            &[
                ("No Delay".to_string(), 800),
                ("Backhaul".to_string(), 120),
                ("RF".to_string(), 80),
            ],
            1000,
            0,
            Some(3),
            &policy,
        );
        let (val, frac) = s.sentinel.expect("sentinel should fire at 80%");
        assert_eq!(val, "No Delay");
        assert!((frac - 0.8).abs() < 1e-4);
    }

    #[test]
    fn sentinel_skips_balanced_distribution() {
        let policy = SamplingPolicy::default_policy();
        let s = categorical_sample(
            &[
                ("a".to_string(), 30),
                ("b".to_string(), 30),
                ("c".to_string(), 40),
            ],
            100,
            0,
            Some(3),
            &policy,
        );
        assert!(s.sentinel.is_none());
    }

    #[test]
    fn numeric_p95() {
        let mut v: Vec<f64> = (1..=100).map(|x| x as f64).collect();
        let s = numeric_sample(&mut v, 0);
        assert_eq!(s.stats.min.as_deref(), Some("1"));
        assert_eq!(s.stats.max.as_deref(), Some("100"));
        assert_eq!(s.stats.p50.as_deref(), Some("50"));
        assert_eq!(s.stats.p95.as_deref(), Some("95"));
    }

    #[test]
    fn default_pii_redacts_email_column() {
        let policy = RedactionPolicy::default_pii();
        assert!(policy.should_redact("email_address", None));
        // Substring match is case-insensitive.
        assert!(policy.should_redact("USER_Email", None));
    }

    #[test]
    fn default_pii_redacts_pii_comment() {
        let policy = RedactionPolicy::default_pii();
        // Innocuous-looking column name, but the comment flags it.
        assert!(policy.should_redact("notes", Some("personally_identifiable")));
        // Also case-insensitive on the comment side.
        assert!(policy.should_redact("notes", Some("Contains PII flag")));
    }

    #[test]
    fn default_pii_does_not_redact_status() {
        let policy = RedactionPolicy::default_pii();
        assert!(!policy.should_redact("status", None));
        assert!(!policy.should_redact("status", Some("workflow state")));
    }

    #[test]
    fn default_policy_enables_redaction() {
        // The default sampling policy must opt callers into PII safety so
        // we don't accidentally ship sample values for `password` columns.
        let p = SamplingPolicy::default_policy();
        assert!(p.redaction.is_some());
        assert!(p
            .redaction
            .as_ref()
            .unwrap()
            .should_redact("password", None));
    }
}
