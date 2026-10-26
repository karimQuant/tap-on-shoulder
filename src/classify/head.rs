//! Logistic-regression head (`assets/head.json`) and the lean-alert decision
//! rule (contract.md, "Model input and head").

use crate::contract::{Category, Head, EMBED_DIM, FEATURE_SPEC, N_FEATURES};

/// `assets/head.json`, embedded at compile time.
pub const HEAD_JSON: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/head.json"));

pub const DIM_EMBED: usize = 2 * EMBED_DIM + N_FEATURES;
pub const DIM_FEATURES: usize = N_FEATURES;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Variant {
    Embed,
    Features,
}

/// Why a head was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeadError {
    Parse,
    Version,
    FeatureSpec,
    Variant,
    Dim,
    Classes,
    Shape,
    NonFinite,
    Threshold,
}

/// A validated head.
#[derive(Debug, Clone)]
pub struct LinearHead {
    pub variant: Variant,
    pub w: Vec<Vec<f32>>,
    pub b: [f32; 4],
    pub lean_threshold: f32,
}

impl LinearHead {
    pub fn parse(json: &str) -> Result<Self, HeadError> {
        let h: Head = serde_json::from_str(json).map_err(|_| HeadError::Parse)?;
        Self::validate(h)
    }

    /// The head embedded in the binary.
    pub fn embedded() -> Result<Self, HeadError> {
        Self::parse(HEAD_JSON)
    }

    pub fn validate(h: Head) -> Result<Self, HeadError> {
        if h.version != 1 {
            return Err(HeadError::Version);
        }
        if h.feature_spec != FEATURE_SPEC {
            return Err(HeadError::FeatureSpec);
        }
        let (variant, dim) = match h.variant.as_str() {
            "embed" => (Variant::Embed, DIM_EMBED),
            "features" => (Variant::Features, DIM_FEATURES),
            _ => return Err(HeadError::Variant),
        };
        if h.dim != dim {
            return Err(HeadError::Dim);
        }
        let expected: Vec<&str> = Category::MODEL_CLASSES.iter().map(|c| c.as_str()).collect();
        if h.classes != expected {
            return Err(HeadError::Classes);
        }
        if h.w.len() != 4 || h.w.iter().any(|r| r.len() != dim) || h.b.len() != 4 {
            return Err(HeadError::Shape);
        }
        if h.w.iter().flatten().chain(&h.b).any(|x| !x.is_finite()) {
            return Err(HeadError::NonFinite);
        }
        if !(h.lean_threshold.is_finite() && (0.0..=1.0).contains(&h.lean_threshold)) {
            return Err(HeadError::Threshold);
        }
        Ok(LinearHead {
            variant,
            b: [h.b[0], h.b[1], h.b[2], h.b[3]],
            w: h.w,
            lean_threshold: h.lean_threshold,
        })
    }

    pub fn dim(&self) -> usize {
        self.w[0].len()
    }

    /// True if any weight or bias is non-zero (an all-zero head is a placeholder).
    pub fn is_trained(&self) -> bool {
        self.w.iter().flatten().chain(&self.b).any(|&x| x != 0.0)
    }

    /// `softmax(W·x + b)` in f32 with max-subtraction.
    pub fn probs(&self, x: &[f32]) -> [f32; 4] {
        assert_eq!(x.len(), self.dim(), "head input dimension");
        let mut logits = [0f32; 4];
        for (k, row) in self.w.iter().enumerate() {
            let mut z = self.b[k];
            for (w, v) in row.iter().zip(x) {
                z += w * v;
            }
            logits[k] = z;
        }
        softmax(logits)
    }
}

pub fn softmax(logits: [f32; 4]) -> [f32; 4] {
    let m = logits.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let mut e = logits.map(|z| (z - m).exp());
    let s: f32 = e.iter().sum();
    for v in e.iter_mut() {
        *v /= s;
    }
    e
}

fn argmax(p: &[f32]) -> usize {
    let mut best = 0;
    for (i, &v) in p.iter().enumerate() {
        if v > p[best] {
            best = i;
        }
    }
    best
}

/// Decision rule. With `lean_threshold = Some(t)` (lean = alert): if
/// `p[0] + p[1] ≥ t`, the higher of classes 0 and 1 wins; otherwise argmax.
/// `None` (lean = balanced) is always argmax. Ties go to the lower index.
pub fn decide(p: &[f32; 4], lean_threshold: Option<f32>) -> Category {
    let idx = match lean_threshold {
        Some(t) if p[0] + p[1] >= t => argmax(&p[..2]),
        _ => argmax(p),
    };
    Category::MODEL_CLASSES[idx]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn head(variant: &str, dim: usize, w0: f32) -> String {
        let mut w = vec![vec![0f32; dim]; 4];
        w[0][0] = w0;
        serde_json::json!({"version":1,"feature_spec":1,"variant":variant,"dim":dim,
            "classes":["needs_decision","blocked","done","info"],"W":w,"b":[0,0,0,0],
            "lean_threshold":0.25,"model_rev":"x"})
        .to_string()
    }

    #[test]
    fn validates_shapes() {
        assert!(LinearHead::parse(&head("features", 12, 0.0)).is_ok());
        assert!(LinearHead::parse(&head("embed", 524, 0.0)).is_ok());
        assert_eq!(
            LinearHead::parse(&head("embed", 12, 0.0)).unwrap_err(),
            HeadError::Dim
        );
        assert_eq!(
            LinearHead::parse(&head("other", 12, 0.0)).unwrap_err(),
            HeadError::Variant
        );
        assert_eq!(LinearHead::parse("{").unwrap_err(), HeadError::Parse);
        let bad = head("features", 12, 0.0).replace("\"done\"", "\"x\"");
        assert_eq!(LinearHead::parse(&bad).unwrap_err(), HeadError::Classes);
    }

    #[test]
    fn embedded_head_is_valid() {
        LinearHead::embedded().expect("assets/head.json validates");
    }

    #[test]
    fn softmax_and_trained() {
        let h = LinearHead::parse(&head("features", 12, 0.0)).unwrap();
        assert!(!h.is_trained());
        assert_eq!(h.probs(&[0.0; 12]), [0.25; 4]);
        let h = LinearHead::parse(&head("features", 12, 2.0)).unwrap();
        assert!(h.is_trained());
        let p = h.probs(&[1.0; 12]);
        assert!((p.iter().sum::<f32>() - 1.0).abs() < 1e-6);
        assert!(p[0] > p[1]);
        let big = softmax([1000.0, 0.0, 0.0, 0.0]);
        assert!(big[0].is_finite() && (big[0] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn lean_alert_rule() {
        let p = [0.15, 0.12, 0.6, 0.13];
        assert_eq!(decide(&p, Some(0.25)), Category::NeedsDecision);
        assert_eq!(decide(&p, None), Category::Done);
        let p = [0.05, 0.21, 0.6, 0.14];
        assert_eq!(decide(&p, Some(0.25)), Category::Blocked);
        let p = [0.1, 0.1, 0.6, 0.2];
        assert_eq!(decide(&p, Some(0.25)), Category::Done);
    }
}
