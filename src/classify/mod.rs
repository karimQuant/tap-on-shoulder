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

use crate::contract::{Classification, Classifier, EMBED_DIM};
use head::{LinearHead, Variant};

pub use rules::RulesClassifier;

/// Best available classifier for this build. Honors `TAP_FORCE_NO_MODEL=1`
/// (tests) by returning the rules-only classifier.
pub fn load() -> Box<dyn Classifier + Send + Sync> {
    if std::env::var("TAP_FORCE_NO_MODEL").as_deref() == Ok("1") {
        return Box::new(RulesClassifier);
    }
    load_model().unwrap_or_else(|| Box::new(RulesClassifier))
}

/// The head-based classifier for this build, ignoring `TAP_FORCE_NO_MODEL`:
/// `embed` when the model is embedded and the head is `embed`, `features`
/// when the head is `features` and trained, otherwise `None`.
pub fn load_model() -> Option<Box<dyn Classifier + Send + Sync>> {
    let head = LinearHead::embedded().ok()?;
    if head.variant == Variant::Features && !head.is_trained() {
        return None;
    }
    with_head(head)
}

/// A classifier for an arbitrary validated head: `embed` heads need the
/// embedded model (else `None`), `features` heads always work.
pub fn with_head(head: LinearHead) -> Option<Box<dyn Classifier + Send + Sync>> {
    match head.variant {
        Variant::Embed => embed_classifier(head),
        Variant::Features => Some(Box::new(FeaturesClassifier { head })),
    }
}

#[cfg(has_model)]
fn embed_classifier(head: LinearHead) -> Option<Box<dyn Classifier + Send + Sync>> {
    Some(Box::new(EmbedClassifier { head }))
}

#[cfg(not(has_model))]
fn embed_classifier(_head: LinearHead) -> Option<Box<dyn Classifier + Send + Sync>> {
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

fn classify_with(head: &LinearHead, message: &str) -> Classification {
    let text = features::prepare_text(message);
    let summary = crate::sanitize::summary(text);
    match head_input(text, head.variant) {
        Some(x) => {
            let p = head.probs(&x);
            Classification {
                category: head::decide(&p, Some(head.lean_threshold)),
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

/// Logistic head over the 12 hand features (option D).
pub struct FeaturesClassifier {
    pub head: LinearHead,
}

impl Classifier for FeaturesClassifier {
    fn classify(&self, text: &str) -> Classification {
        classify_with(&self.head, text)
    }

    fn variant(&self) -> &'static str {
        "features"
    }
}

/// Logistic head over static embeddings plus hand features (option A).
#[cfg(has_model)]
pub struct EmbedClassifier {
    pub head: LinearHead,
}

#[cfg(has_model)]
impl Classifier for EmbedClassifier {
    fn classify(&self, text: &str) -> Classification {
        classify_with(&self.head, text)
    }

    fn variant(&self) -> &'static str {
        "embed"
    }
}
