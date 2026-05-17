//! Heuristic PII detection on sample values. Augments the keyword-based
//! RedactionPolicy by inspecting the actual value distribution.

use crate::model::Column;
use once_cell::sync::Lazy;
use regex::Regex;

static EMAIL_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^[^@\s]+@[^@\s]+\.[^@\s]+$").unwrap());
static PHONE_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^[+]?[\d\s\-()]{7,}$").unwrap());
static SSN_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"^\d{3}-?\d{2}-?\d{4}$").unwrap());
static CC_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"^[\d\s-]{13,19}$").unwrap());

#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub enum PiiKind {
    Email,
    Phone,
    Ssn,
    CreditCard,
}

impl PiiKind {
    pub fn as_str(self) -> &'static str {
        match self {
            PiiKind::Email => "email",
            PiiKind::Phone => "phone",
            PiiKind::Ssn => "ssn",
            PiiKind::CreditCard => "credit_card",
        }
    }
}

pub fn classify_column(col: &Column) -> Option<PiiKind> {
    let sample = col.sample.as_ref()?;
    let values: Vec<&str> = sample.top_values.iter().map(|(v, _)| v.as_str()).collect();
    if values.is_empty() {
        return None;
    }
    let n = values.len();
    let email_hits = values.iter().filter(|v| EMAIL_RE.is_match(v)).count();
    let phone_hits = values.iter().filter(|v| PHONE_RE.is_match(v)).count();
    let ssn_hits = values.iter().filter(|v| SSN_RE.is_match(v)).count();
    let cc_hits = values.iter().filter(|v| CC_RE.is_match(v)).count();
    let threshold = (n * 2).div_ceil(3); // 2/3 majority
    if email_hits >= threshold {
        return Some(PiiKind::Email);
    }
    if ssn_hits >= threshold {
        return Some(PiiKind::Ssn);
    }
    if cc_hits >= threshold {
        return Some(PiiKind::CreditCard);
    }
    if phone_hits >= threshold {
        return Some(PiiKind::Phone);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ColumnSample, DataType, SampleStats};

    fn make_column(values: Vec<&str>) -> Column {
        Column {
            name: "x".to_string(),
            data_type: DataType::Text,
            native_type: "text".to_string(),
            nullable: true,
            default: None,
            comment: None,
            ordinal: 0,
            sample: Some(ColumnSample {
                stats: SampleStats::default(),
                top_values: values
                    .into_iter()
                    .map(|v| (v.to_string(), 0.5_f32))
                    .collect(),
                sentinel: None,
            }),
            check_constraint: None,
            is_unique: false,
            generation_expression: None,
        }
    }

    #[test]
    fn detects_emails() {
        let col = make_column(vec![
            "alice@example.com",
            "bob@foo.org",
            "carol@bar.net",
        ]);
        assert_eq!(classify_column(&col), Some(PiiKind::Email));
    }

    #[test]
    fn mixed_values_no_classification() {
        let col = make_column(vec!["alice@example.com", "USA", "California"]);
        // Email is 1/3 -- below the 2/3 threshold.
        assert_eq!(classify_column(&col), None);
    }

    #[test]
    fn detects_ssn_shape() {
        let col = make_column(vec!["123-45-6789", "987-65-4321", "111-22-3333"]);
        assert_eq!(classify_column(&col), Some(PiiKind::Ssn));
    }

    #[test]
    fn empty_sample_yields_none() {
        let col = make_column(Vec::new());
        assert_eq!(classify_column(&col), None);
    }

    #[test]
    fn no_sample_yields_none() {
        let mut col = make_column(vec!["a@b.com", "c@d.com"]);
        col.sample = None;
        assert_eq!(classify_column(&col), None);
    }
}
