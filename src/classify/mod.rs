//! Classification: event rules + model. Lane B owns this module.
//!
//! `RulesClassifier` is the contract-step stub every lane can build against:
//! it labels every message `needs_decision` with an empty summary.

use crate::contract::{Category, Classification, Classifier};

pub struct RulesClassifier;

impl Classifier for RulesClassifier {
    fn classify(&self, _text: &str) -> Classification {
        Classification {
            category: Category::NeedsDecision,
            summary: String::new(),
            probs: None,
        }
    }

    fn variant(&self) -> &'static str {
        "rules"
    }
}
