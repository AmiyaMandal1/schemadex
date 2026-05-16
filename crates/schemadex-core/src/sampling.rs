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

#[derive(Debug, Clone, Copy, Default)]
pub struct SamplingPolicy {
    pub top_k: usize,
    pub sentinel_threshold: f32,
    /// Maximum distinct values to keep before bailing out (avoid TEXT columns
    /// with 10M distinct values).
    pub max_distinct: u64,
    pub sample_rows: u64,
}

impl SamplingPolicy {
    pub const fn default_policy() -> Self {
        Self {
            top_k: DEFAULT_TOP_K,
            sentinel_threshold: SENTINEL_THRESHOLD,
            max_distinct: 1_000,
            sample_rows: 10_000,
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
}
