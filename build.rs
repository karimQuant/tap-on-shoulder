//! Embeds the model when `assets/potion-base-8M/` is present (Lane B owns this file).
//!
//! Outputs in `OUT_DIR` (see `docs/design/contract.md`, "Embedding"):
//! - `embeddings.i8`: row-major int8 table, `rows × dim`.
//! - `scales.f32`: one little-endian f32 scale per row.
//! - `vocab.txt`: tokens in id order, one per line.
//! - `vocab_sorted.u32`: little-endian ids sorted by token bytes (binary search index).
//! - `model_meta.rs`: row count, dimension, `[UNK]` id, and special ids.
//!
//! With the assets present it emits `cfg(has_model)`. Without them the build
//! continues rules/features-only, unless `TAP_REQUIRE_MODEL=embed`.

use std::fs;
use std::path::{Path, PathBuf};

const MODEL_DIR: &str = "assets/potion-base-8M";
const TENSOR: &str = "embeddings";

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=assets");
    println!("cargo:rerun-if-env-changed=TAP_REQUIRE_MODEL");
    println!("cargo::rustc-check-cfg=cfg(has_model)");

    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    let dir = manifest.join(MODEL_DIR);
    let st = dir.join("model.safetensors");
    let tok = dir.join("tokenizer.json");
    let require = std::env::var("TAP_REQUIRE_MODEL").unwrap_or_default();

    if !(st.is_file() && tok.is_file()) {
        if require == "embed" {
            panic!(
                "TAP_REQUIRE_MODEL=embed but {MODEL_DIR}/model.safetensors and/or \
                 {MODEL_DIR}/tokenizer.json are missing; run scripts/fetch-model.sh"
            );
        }
        println!("cargo:warning=model assets absent: building without embeddings");
        return;
    }

    let (rows, dim, data) = read_safetensors(&st);
    let vocab = read_vocab(&tok, rows);
    write_quantized(&out, rows, dim, &data);
    write_vocab(&out, &vocab.tokens);
    let meta = format!(
        "pub const ROWS: usize = {rows};\npub const DIM: usize = {dim};\n\
         pub const UNK_ID: u32 = {};\npub const SPECIAL_IDS: &[u32] = &{:?};\n",
        vocab.unk, vocab.special
    );
    fs::write(out.join("model_meta.rs"), meta).expect("write model_meta.rs");
    println!("cargo:rustc-cfg=has_model");
}

/// Parse a safetensors file by hand: u64 LE header length, JSON header, raw data.
fn read_safetensors(path: &Path) -> (usize, usize, Vec<f32>) {
    let bytes = fs::read(path).expect("read model.safetensors");
    assert!(bytes.len() >= 8, "safetensors: file too short");
    let hlen = u64::from_le_bytes(bytes[..8].try_into().unwrap()) as usize;
    assert!(
        8 + hlen <= bytes.len(),
        "safetensors: header length out of range"
    );
    let header: serde_json::Value =
        serde_json::from_slice(&bytes[8..8 + hlen]).expect("safetensors: header JSON");
    let t = header
        .get(TENSOR)
        .unwrap_or_else(|| panic!("safetensors: tensor `{TENSOR}` missing"));
    assert_eq!(t["dtype"], "F32", "safetensors: `{TENSOR}` must be F32");
    let shape: Vec<usize> = t["shape"]
        .as_array()
        .expect("safetensors: shape")
        .iter()
        .map(|v| v.as_u64().expect("shape dim") as usize)
        .collect();
    assert_eq!(shape.len(), 2, "safetensors: `{TENSOR}` must be 2-D");
    let (rows, dim) = (shape[0], shape[1]);
    let off: Vec<usize> = t["data_offsets"]
        .as_array()
        .expect("safetensors: data_offsets")
        .iter()
        .map(|v| v.as_u64().expect("offset") as usize)
        .collect();
    let base = 8 + hlen;
    let (start, end) = (base + off[0], base + off[1]);
    assert!(
        end <= bytes.len() && end - start == rows * dim * 4,
        "safetensors: data size does not match shape"
    );
    let data = bytes[start..end]
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
        .collect();
    (rows, dim, data)
}

struct Vocab {
    tokens: Vec<String>,
    unk: u32,
    special: Vec<u32>,
}

fn read_vocab(path: &Path, rows: usize) -> Vocab {
    let j: serde_json::Value =
        serde_json::from_slice(&fs::read(path).expect("read tokenizer.json"))
            .expect("tokenizer.json");
    let model = &j["model"];
    assert_eq!(
        model["type"], "WordPiece",
        "tokenizer.json: expected WordPiece"
    );
    let map = model["vocab"]
        .as_object()
        .expect("tokenizer.json: model.vocab");
    let mut tokens: Vec<Option<String>> = vec![None; map.len()];
    for (tok, id) in map {
        let id = id.as_u64().expect("vocab id") as usize;
        assert!(
            id < tokens.len() && tokens[id].is_none(),
            "vocab ids must be dense and unique"
        );
        assert!(!tok.contains('\n'), "vocab token contains a newline");
        tokens[id] = Some(tok.clone());
    }
    let tokens: Vec<String> = tokens
        .into_iter()
        .map(|t| t.expect("dense vocab"))
        .collect();
    assert_eq!(tokens.len(), rows, "vocab size must equal embedding rows");
    let unk_tok = model["unk_token"].as_str().expect("unk_token");
    let unk = map[unk_tok].as_u64().expect("unk id") as u32;
    let mut special: Vec<u32> = j["added_tokens"]
        .as_array()
        .expect("added_tokens")
        .iter()
        .filter(|t| t["special"].as_bool() == Some(true))
        .map(|t| t["id"].as_u64().expect("added token id") as u32)
        .collect();
    special.sort_unstable();
    assert!(special.contains(&unk), "[UNK] must be a special token");
    Vocab {
        tokens,
        unk,
        special,
    }
}

/// int8 per-row quantization (contract.md): scale = max|row| / 127 (1 if 0),
/// q = clamp(round_half_away_from_zero(x / scale), -127, 127).
fn write_quantized(out: &Path, rows: usize, dim: usize, data: &[f32]) {
    let mut q = Vec::with_capacity(rows * dim);
    let mut scales = Vec::with_capacity(rows * 4);
    for row in data.chunks_exact(dim) {
        let max = row.iter().fold(0f32, |m, x| m.max(x.abs()));
        let scale = if max == 0.0 { 1.0 } else { max / 127.0 };
        for &x in row {
            // f32::round rounds half away from zero.
            q.push((x / scale).round().clamp(-127.0, 127.0) as i8 as u8);
        }
        scales.extend_from_slice(&scale.to_le_bytes());
    }
    fs::write(out.join("embeddings.i8"), q).expect("write embeddings.i8");
    fs::write(out.join("scales.f32"), scales).expect("write scales.f32");
}

fn write_vocab(out: &Path, tokens: &[String]) {
    fs::write(out.join("vocab.txt"), tokens.join("\n")).expect("write vocab.txt");
    let mut ids: Vec<u32> = (0..tokens.len() as u32).collect();
    ids.sort_by(|&a, &b| {
        tokens[a as usize]
            .as_bytes()
            .cmp(tokens[b as usize].as_bytes())
    });
    let bytes: Vec<u8> = ids.iter().flat_map(|i| i.to_le_bytes()).collect();
    fs::write(out.join("vocab_sorted.u32"), bytes).expect("write vocab_sorted.u32");
}
