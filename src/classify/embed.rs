//! Static sentence embedding: dequantize int8 rows, sum sequentially in f32
//! in id order, divide by n, then L2-normalize (contract.md, "Embedding").

use crate::contract::EMBED_DIM;

/// A quantized embedding table: `rows × dim` int8 values plus one scale per row.
pub struct Table<'a> {
    pub q: &'a [i8],
    pub scales: &'a [f32],
    pub dim: usize,
}

impl Table<'_> {
    pub fn rows(&self) -> usize {
        self.scales.len()
    }

    /// Sentence vector for `ids`. Out-of-range ids are ignored; no ids or a
    /// zero norm gives the zero vector.
    pub fn embed(&self, ids: &[u32]) -> [f32; EMBED_DIM] {
        let mut sum = [0f32; EMBED_DIM];
        debug_assert_eq!(self.dim, EMBED_DIM);
        let mut n = 0usize;
        for &id in ids {
            let r = id as usize;
            if r >= self.rows() {
                continue;
            }
            let scale = self.scales[r];
            let row = &self.q[r * self.dim..(r + 1) * self.dim];
            for (s, &q) in sum.iter_mut().zip(row) {
                *s += q as f32 * scale;
            }
            n += 1;
        }
        if n == 0 {
            return [0.0; EMBED_DIM];
        }
        let nf = n as f32;
        for s in sum.iter_mut() {
            *s /= nf;
        }
        let norm = sum.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm == 0.0 || !norm.is_finite() {
            return [0.0; EMBED_DIM];
        }
        for s in sum.iter_mut() {
            *s /= norm;
        }
        sum
    }
}

#[cfg(has_model)]
mod embedded {
    pub static Q: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/embeddings.i8"));
    pub static SCALES: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/scales.f32"));
}

/// The table embedded by `build.rs`, with scales decoded from little-endian.
#[cfg(has_model)]
pub struct EmbeddedTable {
    scales: Vec<f32>,
}

#[cfg(has_model)]
impl EmbeddedTable {
    pub fn load() -> Self {
        let scales = embedded::SCALES
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect::<Vec<_>>();
        assert_eq!(scales.len(), super::wordpiece::MODEL_ROWS);
        assert_eq!(super::wordpiece::MODEL_DIM, EMBED_DIM);
        EmbeddedTable { scales }
    }

    pub fn table(&self) -> Table<'_> {
        // SAFETY: i8 and u8 have identical size and alignment; every bit pattern is valid.
        let q = unsafe {
            std::slice::from_raw_parts(embedded::Q.as_ptr().cast::<i8>(), embedded::Q.len())
        };
        Table {
            q,
            scales: &self.scales,
            dim: EMBED_DIM,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mean_then_normalize() {
        let mut q = vec![0i8; 2 * EMBED_DIM];
        q[0] = 127;
        q[EMBED_DIM + 1] = -127;
        let scales = [1.0 / 127.0, 2.0 / 127.0];
        let t = Table {
            q: &q,
            scales: &scales,
            dim: EMBED_DIM,
        };
        let v = t.embed(&[0, 1]);
        let n = (1f32 + 4.0).sqrt();
        assert!((v[0] - 1.0 / n).abs() < 1e-6);
        assert!((v[1] + 2.0 / n).abs() < 1e-6);
        assert_eq!(t.embed(&[]), [0.0; EMBED_DIM]);
        assert_eq!(t.embed(&[99]), [0.0; EMBED_DIM]);
    }
}
