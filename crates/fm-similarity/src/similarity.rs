//! 余弦相似度。PRD §7.2 cosine_sim(query, vector)。
//!
//! 纯 Rust, 无 NEON intrinsic 显式调用 —— 编译器自动矢化 f32 点积。
//! 输入需同长, 长度不符返回 None (上层判错, 不静默截断)。

/// 两向量余弦相似度。零向量返回 0.0 (不除零)。
/// 返回 [-1.0, 1.0]。
pub fn cosine(a: &[f32], b: &[f32]) -> Option<f64> {
    if a.len() != b.len() {
        return None;
    }
    let mut dot = 0.0f32;
    let mut na = 0.0f32;
    let mut nb = 0.0f32;
    for i in 0..a.len() {
        dot += a[i] * b[i];
        na += a[i] * a[i];
        nb += b[i] * b[i];
    }
    let denom = (na * nb).sqrt();
    if denom == 0.0 {
        return Some(0.0);
    }
    Some((dot / denom) as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_vectors_cosine_one() {
        let v = vec![1.0, 2.0, 3.0, 4.0];
        let c = cosine(&v, &v).unwrap();
        assert!((c - 1.0).abs() < 1e-5, "cos={c}");
    }

    #[test]
    fn orthogonal_vectors_cosine_zero() {
        let a = vec![1.0, 0.0];
        let b = vec![0.0, 1.0];
        let c = cosine(&a, &b).unwrap();
        assert!(c.abs() < 1e-5, "cos={c}");
    }

    #[test]
    fn opposite_vectors_cosine_neg_one() {
        let a = vec![1.0, 1.0, 1.0];
        let b = vec![-1.0, -1.0, -1.0];
        let c = cosine(&a, &b).unwrap();
        assert!((c + 1.0).abs() < 1e-5, "cos={c}");
    }

    #[test]
    fn dim_mismatch_returns_none() {
        let a = vec![1.0, 2.0];
        let b = vec![1.0];
        assert!(cosine(&a, &b).is_none());
    }

    #[test]
    fn zero_vector_returns_zero() {
        let a = vec![0.0, 0.0, 0.0];
        let b = vec![1.0, 2.0, 3.0];
        let c = cosine(&a, &b).unwrap();
        assert!(c.abs() < 1e-5, "cos={c}");
    }

    #[test]
    fn cosine_in_range() {
        let a = vec![1.0, 0.5, 0.2];
        let b = vec![0.9, 0.3, 0.7];
        let c = cosine(&a, &b).unwrap();
        assert!((-1.0..=1.0).contains(&c), "cos={c}");
    }

    #[test]
    fn empty_vectors_zero() {
        let c = cosine(&[], &[]).unwrap();
        assert!(c.abs() < 1e-5);
    }
}
