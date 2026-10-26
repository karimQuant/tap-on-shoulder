//! Rust ↔ Python parity (contract.md, "Parity fixtures").
//!
//! - `tests/fixtures/parity.json` (from `model/export.py`): ids, features,
//!   probabilities, and labels must match. Skips with a message until it exists.
//! - `tests/fixtures/wordpiece_golden.json`: token ids from HF `tokenizers` for
//!   synthetic strings. `TAP_WORDPIECE_GOLDEN=<file>` checks an extra file of
//!   the same shape (used for large fuzz runs).

use std::path::{Path, PathBuf};

use serde::Deserialize;
use tap::classify::{self, features, head};
use tap::contract::ParityFixture;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn cosine(a: &[f32], b: &[f32]) -> f64 {
    let dot: f64 = a.iter().zip(b).map(|(x, y)| *x as f64 * *y as f64).sum();
    let na: f64 = a.iter().map(|x| (*x as f64).powi(2)).sum::<f64>().sqrt();
    let nb: f64 = b.iter().map(|x| (*x as f64).powi(2)).sum::<f64>().sqrt();
    if na == 0.0 && nb == 0.0 {
        return 1.0;
    }
    dot / (na * nb)
}

#[test]
fn parity_fixtures() {
    let path = fixture("parity.json");
    if !path.exists() {
        eprintln!("SKIP parity_fixtures: tests/fixtures/parity.json not present yet");
        return;
    }
    let fixtures: Vec<ParityFixture> =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).expect("parity.json");
    assert!(!fixtures.is_empty(), "parity.json is empty");
    let h = head::LinearHead::embedded().expect("assets/head.json");
    let need_model = h.variant == head::Variant::Embed;
    if classify::model_status() != "embedded" {
        assert!(
            !need_model,
            "head variant `embed` needs the model: fetch assets/potion-base-8M first"
        );
        eprintln!("NOTE parity_fixtures: model absent, token ids not checked");
    }
    for (i, fx) in fixtures.iter().enumerate() {
        let text = features::prepare_text(&fx.text);
        if let Some(ids) = classify::token_ids(features::tail_of(text)) {
            assert_eq!(ids, fx.ids_tail, "fixture {i}: ids_tail");
            assert_eq!(
                classify::token_ids(text).unwrap(),
                fx.ids_text,
                "fixture {i}: ids_text"
            );
        }
        let f = features::compute(text);
        let cos = cosine(&f, &fx.features);
        assert!(cos > 0.9999, "fixture {i}: feature cosine {cos}");
        let x = classify::head_input(text, h.variant).expect("head input");
        let p = h.probs(&x);
        assert_eq!(fx.probs.len(), 4, "fixture {i}: probs length");
        for (k, (a, b)) in p.iter().zip(&fx.probs).enumerate() {
            let d = (a - b).abs();
            assert!(d < 1e-3, "fixture {i}: |Δp[{k}]| = {d}");
        }
        let label = head::decide(&p, Some(h.lean_threshold));
        assert_eq!(label.as_str(), fx.label, "fixture {i}: label");
    }
    eprintln!("parity_fixtures: {} fixtures OK", fixtures.len());
}

#[derive(Deserialize)]
struct Golden {
    text: String,
    ids: Vec<u32>,
}

fn check_golden(path: &Path) -> usize {
    let entries: Vec<Golden> =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).expect("golden json");
    let mut bad = Vec::new();
    for (i, g) in entries.iter().enumerate() {
        let got = classify::token_ids(&g.text).expect("model embedded");
        if got != g.ids {
            bad.push(i);
            if bad.len() <= 5 {
                eprintln!(
                    "mismatch {i}: {:?}\n  hf:   {:?}\n  rust: {:?}",
                    g.text, g.ids, got
                );
            }
        }
    }
    assert!(
        bad.is_empty(),
        "{} / {} golden strings differ from HF",
        bad.len(),
        entries.len()
    );
    entries.len()
}

#[test]
fn wordpiece_matches_hf_golden() {
    if classify::model_status() != "embedded" {
        eprintln!("SKIP wordpiece_matches_hf_golden: model assets absent");
        return;
    }
    let n = check_golden(&fixture("wordpiece_golden.json"));
    assert!(n >= 30);
    eprintln!("wordpiece golden: {n}/{n} match HF");
    if let Ok(extra) = std::env::var("TAP_WORDPIECE_GOLDEN") {
        let n = check_golden(Path::new(&extra));
        eprintln!("wordpiece extra golden: {n}/{n} match HF");
    }
}
