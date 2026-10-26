//! Classification of the final assistant message. Lane B owns this module.
//!
//! Cross-lane interface (do not change signatures without Lead):
//! - [`load`] returns the best available classifier (embedded model, features
//!   head, or rules).
//!
//! The message text is untrusted. Every classifier returns only a label, the
//! head probabilities (if a model ran), and an extractive sanitized summary.

pub mod embed;
pub mod features;
pub mod head;
pub mod rules;
mod unicode_tables;
pub mod wordpiece;

pub use crate::config::{ClassifierMode, Lean};
use crate::contract::{Classification, Classifier, EMBED_DIM};
use head::{LinearHead, Variant};

pub use rules::RulesClassifier;

/// Best available classifier with the default settings
/// (`classifier.mode = "model"`, `classifier.lean = "alert"`).
pub fn load() -> Box<dyn Classifier + Send + Sync> {
    load_with(ClassifierMode::Model, Lean::Alert)
}

/// The classifier for the user's `classifier.mode` and `classifier.lean`.
/// `Rules` (or `TAP_FORCE_NO_MODEL=1`, used by tests) returns the rules-only
/// classifier; `Model` returns the head-based classifier when the build has
/// one, else the rules. `lean` only affects head-based classifiers.
pub fn load_with(mode: ClassifierMode, lean: Lean) -> Box<dyn Classifier + Send + Sync> {
    if mode == ClassifierMode::Rules || std::env::var("TAP_FORCE_NO_MODEL").as_deref() == Ok("1") {
        return Box::new(RulesClassifier);
    }
    load_model(lean).unwrap_or_else(|| Box::new(RulesClassifier))
}

/// The head-based classifier for this build, ignoring `TAP_FORCE_NO_MODEL`:
/// `embed` when the model is embedded and the head is `embed`, `features`
/// when the head is `features` and trained, otherwise `None`.
pub fn load_model(lean: Lean) -> Option<Box<dyn Classifier + Send + Sync>> {
    let head = LinearHead::embedded().ok()?;
    if head.variant == Variant::Features && !head.is_trained() {
        return None;
    }
    with_head_lean(head, lean)
}

/// A classifier for an arbitrary validated head with `lean = "alert"`.
pub fn with_head(head: LinearHead) -> Option<Box<dyn Classifier + Send + Sync>> {
    with_head_lean(head, Lean::Alert)
}

/// A classifier for an arbitrary validated head: `embed` heads need the
/// embedded model (else `None`), `features` heads always work. `Alert` uses
/// the head's `lean_threshold`; `Balanced` uses plain argmax.
pub fn with_head_lean(head: LinearHead, lean: Lean) -> Option<Box<dyn Classifier + Send + Sync>> {
    let threshold = match lean {
        Lean::Alert => Some(head.lean_threshold),
        Lean::Balanced => None,
    };
    match head.variant {
        Variant::Embed => embed_classifier(head, threshold),
        Variant::Features => Some(Box::new(FeaturesClassifier { head, threshold })),
    }
}

#[cfg(has_model)]
fn embed_classifier(
    head: LinearHead,
    threshold: Option<f32>,
) -> Option<Box<dyn Classifier + Send + Sync>> {
    Some(Box::new(EmbedClassifier { head, threshold }))
}

#[cfg(not(has_model))]
fn embed_classifier(
    _head: LinearHead,
    _threshold: Option<f32>,
) -> Option<Box<dyn Classifier + Send + Sync>> {
    None
}

/// `embedded` if `build.rs` embedded the model, else `absent`.
pub fn model_status() -> &'static str {
    if cfg!(has_model) {
        "embedded"
    } else {
        "absent"
    }
}

#[cfg(has_model)]
struct Model {
    vocab: wordpiece::EmbeddedVocab,
    table: embed::EmbeddedTable,
}

#[cfg(has_model)]
fn model() -> &'static Model {
    static M: std::sync::OnceLock<Model> = std::sync::OnceLock::new();
    M.get_or_init(|| Model {
        vocab: wordpiece::EmbeddedVocab::load(),
        table: embed::EmbeddedTable::load(),
    })
}

/// Token ids (contract step 6) with the embedded vocab, or `None` without a model.
pub fn token_ids(text: &str) -> Option<Vec<u32>> {
    #[cfg(has_model)]
    {
        Some(wordpiece::encode(text, &model().vocab))
    }
    #[cfg(not(has_model))]
    {
        let _ = text;
        None
    }
}

/// Token strings for `text` (after step 6), or `None` without a model.
pub fn token_strings(text: &str) -> Option<Vec<&'static str>> {
    #[cfg(has_model)]
    {
        let m = model();
        Some(
            wordpiece::encode(text, &m.vocab)
                .into_iter()
                .filter_map(|id| m.vocab.token(id))
                .collect(),
        )
    }
    #[cfg(not(has_model))]
    {
        let _ = text;
        None
    }
}

/// Sentence embedding of `text` with the embedded model, or `None` without one.
pub fn embedding(text: &str) -> Option<[f32; EMBED_DIM]> {
    #[cfg(has_model)]
    {
        let m = model();
        Some(m.table.table().embed(&wordpiece::encode(text, &m.vocab)))
    }
    #[cfg(not(has_model))]
    {
        let _ = text;
        None
    }
}

/// Head input for `message` and `variant`: `[emb(tail), emb(text), f0..f11]`
/// (524-d) or `[f0..f11]` (12-d). `None` if `embed` is asked without a model.
pub fn head_input(message: &str, variant: Variant) -> Option<Vec<f32>> {
    let text = features::prepare_text(message);
    let f = features::compute(text);
    match variant {
        Variant::Features => Some(f.to_vec()),
        Variant::Embed => {
            let mut x = Vec::with_capacity(head::DIM_EMBED);
            x.extend_from_slice(&embedding(features::tail_of(text))?);
            x.extend_from_slice(&embedding(text)?);
            x.extend_from_slice(&f);
            Some(x)
        }
    }
}

fn classify_with(head: &LinearHead, threshold: Option<f32>, message: &str) -> Classification {
    let text = features::prepare_text(message);
    let summary = crate::sanitize::summary(text);
    match head_input(text, head.variant) {
        Some(x) => {
            let p = head.probs(&x);
            Classification {
                category: head::decide(&p, threshold),
                summary,
                probs: Some(p),
            }
        }
        None => Classification {
            category: rules::label(&features::compute(text)),
            summary,
            probs: None,
        },
    }
}

/// Logistic head over the 12 hand features (option D). `threshold` is the
/// lean-alert threshold, or `None` for plain argmax.
pub struct FeaturesClassifier {
    pub head: LinearHead,
    pub threshold: Option<f32>,
}

impl Classifier for FeaturesClassifier {
    fn classify(&self, text: &str) -> Classification {
        classify_with(&self.head, self.threshold, text)
    }

    fn variant(&self) -> &'static str {
        "features"
    }
}

/// Logistic head over static embeddings plus hand features (option A).
#[cfg(has_model)]
pub struct EmbedClassifier {
    pub head: LinearHead,
    pub threshold: Option<f32>,
}

#[cfg(has_model)]
impl Classifier for EmbedClassifier {
    fn classify(&self, text: &str) -> Classification {
        classify_with(&self.head, self.threshold, text)
    }

    fn variant(&self) -> &'static str {
        "embed"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::Category;

    /// A features head whose probabilities are `p` for every input
    /// (zero weights, log-probability biases).
    fn constant_head(p: [f32; 4], lean_threshold: f32) -> LinearHead {
        LinearHead {
            variant: Variant::Features,
            w: vec![vec![0.0; head::DIM_FEATURES]; 4],
            b: p.map(f32::ln),
            lean_threshold,
        }
    }

    #[test]
    fn mode_rules_forces_rules_classifier() {
        for lean in [Lean::Alert, Lean::Balanced] {
            let c = load_with(ClassifierMode::Rules, lean);
            assert_eq!(c.variant(), "rules");
            assert!(c.classify("Should I use A or B?").probs.is_none());
        }
    }

    #[test]
    fn lean_alert_vs_balanced() {
        // p0 + p1 = 0.27 ≥ 0.25, but `done` is the argmax.
        let p = [0.15, 0.12, 0.6, 0.13];
        let alert = with_head_lean(constant_head(p, 0.25), Lean::Alert).unwrap();
        let balanced = with_head_lean(constant_head(p, 0.25), Lean::Balanced).unwrap();
        let a = alert.classify("All tests pass.");
        let b = balanced.classify("All tests pass.");
        assert_eq!(a.category, Category::NeedsDecision);
        assert_eq!(b.category, Category::Done);
        assert_eq!(a.probs, b.probs);
        // `with_head` defaults to alert.
        let d = with_head(constant_head(p, 0.25)).unwrap();
        assert_eq!(
            d.classify("All tests pass.").category,
            Category::NeedsDecision
        );
    }
}
