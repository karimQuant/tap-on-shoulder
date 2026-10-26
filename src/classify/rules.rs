//! Rules-only classifier over the hand features: the no-model fallback.

use super::features::{self, Features};
use super::features::{F_BLOCKER, F_COMPLETION, F_DECISION, F_ERROR_MARKER, F_LAST_LINE_QUESTION};
use super::features::{F_OFFER, F_OPTIONS_LIST};
use crate::contract::{Category, Classification, Classifier};

/// Heuristic label from features, leaning towards `needs_decision`.
pub fn label(f: &Features) -> Category {
    let on = |i: usize| f[i] > 0.5;
    let asks = on(F_DECISION) || on(F_OPTIONS_LIST) || on(F_LAST_LINE_QUESTION);
    let optional_offer = on(F_OFFER) && !on(F_DECISION);
    if asks && !optional_offer {
        Category::NeedsDecision
    } else if on(F_BLOCKER) || on(F_ERROR_MARKER) {
        Category::Blocked
    } else if on(F_COMPLETION) {
        Category::Done
    } else if on(F_OFFER) {
        Category::Info
    } else {
        Category::NeedsDecision
    }
}

pub struct RulesClassifier;

impl Classifier for RulesClassifier {
    fn classify(&self, text: &str) -> Classification {
        let text = features::prepare_text(text);
        Classification {
            category: label(&features::compute(text)),
            summary: crate::sanitize::summary(text),
            probs: None,
        }
    }

    fn variant(&self) -> &'static str {
        "rules"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cat(s: &str) -> Category {
        RulesClassifier.classify(s).category
    }

    #[test]
    fn labels() {
        assert_eq!(
            cat("Should I keep the legacy column or drop it?"),
            Category::NeedsDecision
        );
        assert_eq!(cat("Done. All tests pass."), Category::Done);
        assert_eq!(
            cat("I can't continue: the STRIPE_KEY env var is missing."),
            Category::Blocked
        );
        assert_eq!(
            cat("Updated the parser. Let me know if you want me to also update the docs?"),
            Category::Info
        );
        assert_eq!(
            cat("Here is a summary of the module."),
            Category::NeedsDecision
        );
        assert_eq!(
            cat("Do you want me to proceed? Happy to adjust."),
            Category::NeedsDecision
        );
    }
}
