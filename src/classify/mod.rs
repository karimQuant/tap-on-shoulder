//! Classification: event rules + model. Lane B owns this module.
//!
//! Cross-lane interface (do not change signatures without Lead):
//! - [`load`] returns the best available classifier (embedded model, features
//!   head, or rules).
//!
//! `RulesClassifier` is the contract-step stub: it labels every message
//! `needs_decision` with the sanitized summary.

use crate::contract::{Category, Classification, Classifier};

/// Best available classifier for this build. Honors `TAP_FORCE_NO_MODEL=1`
/// (tests) by returning the rules-only classifier.
pub fn load() -> Box<dyn Classifier + Send + Sync> {
    Box::new(RulesClassifier)
}

pub struct RulesClassifier;

impl Classifier for RulesClassifier {
    fn classify(&self, text: &str) -> Classification {
        Classification {
            category: Category::NeedsDecision,
            summary: crate::sanitize::summary(text),
            probs: None,
        }
    }

    fn variant(&self) -> &'static str {
        "rules"
    }
}
