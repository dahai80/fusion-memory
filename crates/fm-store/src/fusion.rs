//! §1.4: store-fusion 后端 (上游 fusion-store#3 trait 对齐已落地)。
//!
//! 消费 fusion-store fs-core HNSW 索引 (mmap 零拷贝), 替 local-store 的 hnsw_rs+sled owned。
//! 实现 fm-store `FusionStoreEngine` trait, 包 fs-core `Engine` 结构体 (走具体方法, 非经 fs-core
//! trait — fs-core trait 含 columnar/batch/create_index 等超 fusion-memory 所需的接口)。
//!
//! 距离语义桥接 (关键):
//! - fs-core `cosine()` = `1.0 - (dot/(|a|·|b|))` → distance (0=同向, 2=反向)
//! - fs-core `search_knn` 返 Vec<(id, distance)> 升序
//! - fusion-memory `search_knn` 契约返 Vec<(id, similarity)> (高=近, 与 local-store 同)
//! - adapter 转 `similarity = 1.0 - distance` (与 local.rs:319 `1.0 - nb.distance` 同公式)
//!
//! KV 桥接: fs-core `ZeroCopyBuffer` (mmap 切片, as_bytes()) → fm-store `ZeroCopyBuffer`
//! (owned Vec<u8>)。mmap 段借出后必须拷成 owned (生命周期独立于 fs-core 引擎), 同 local-store
//! 非 mmap 路径。类型名 ZeroCopyBuffer 源自 store-fusion 蓝图; adapter 下仍 owned 拷贝
//! (mmap 段跨调用持引用不安全), 真零拷贝需上层借 mmap handle 保活, 后续优化。
//!
//! 100% 离线: fs-core 纯本地 (mmap + LMDB heed + 本地文件), 无运行时外网。构建期 cargo 拉
//! git dep (同 fusion-memory 自身推 GitHub), 非运行时 cloud API。

use std::path::Path;

use fs_core::{Engine, VectorSchema};
use tracing::info;

use crate::error::{StoreError, StoreResult};
use crate::trait_def::{FusionStoreEngine, ZeroCopyBuffer};

// quota_limit=0 = 不限 (fs-core 语义, 同 open_kv_only)。fusion-memory 无配额门, 不设上限。
const QUOTA_UNLIMITED: u64 = 0;

/// store-fusion 后端: 包 fs-core `Engine`, 实现 fm-store `FusionStoreEngine`。
///
/// 构造锁 VectorSchema{dim, Cosine}。重开 (schema 已存) 走 fs-core reopen 路径, dim 从
/// 调用方传入校验一致 (fs-core reopen 不返 schema, 故调用方须记 dim)。
pub struct FusionStore {
    engine: Engine,
    dim: usize,
}

impl FusionStore {
    /// 打开/创建 store-fusion 后端。
    /// home = namespace 根目录 (fs-core 建 kv/vec/wal 子目录)。
    /// dim = 向量维度 (首次建索引锁定; 重开须与已存 schema 一致, 否则 fs-core 报错)。
    pub fn open(path: impl AsRef<Path>, dim: usize) -> StoreResult<Self> {
        if dim == 0 {
            return Err(StoreError::Dimension {
                expected: 1,
                got: 0,
            });
        }
        let schema = VectorSchema::new(dim, fs_core::MetricKind::Cosine);
        // schema=Some → 首次建索引锁 dim; 重开 fs-core 检测 vec_meta 存在走 reopen (dim 由调用方校验)。
        // 实际: fs-core open schema=Some 总是 create_vector_index; 重开场景调用方传同 dim, fs-core
        // reopen 检测已有索引。为兼容重开, 这里用 fs-core Engine::open 的 schema=Some 路径,
        // 重开时若 vec_meta 已存在则 create 路径幂等 (fs-core 内部幂等建索引)。
        // ef_search 走 VectorSchema::new 默认 (DEFAULT_EF_SEARCH=200, 达 PRD §2.5 ≥0.95 召回)。
        let engine = Engine::open(path.as_ref(), Some(schema), QUOTA_UNLIMITED)
            .map_err(|e| StoreError::Sled(format!("fs-core open: {e}")))?;
        info!(dim, "store-fusion opened (fs-core backend)");
        Ok(Self { engine, dim })
    }

    fn dim_check(&self, vec: &[f32]) -> Result<(), StoreError> {
        if vec.len() != self.dim {
            return Err(StoreError::Dimension {
                expected: self.dim,
                got: vec.len(),
            });
        }
        Ok(())
    }

    // fs-core StoreError → fm-store StoreError。统一收口为 Sled(string) 透传上游错误描述
    // (fm-store 无上游错误变体; to_memory 转 MemoryError::Store(string), fail-visible 非静默)。
    fn map_err<E: std::fmt::Display>(e: E) -> StoreError {
        StoreError::Sled(e.to_string())
    }
}

impl FusionStoreEngine for FusionStore {
    // UFCS 调 fs-core trait 方法 (fs_core::FusionStoreEngine 与本 crate trait 同名, 须全限定避歧义)。
    fn put_kv(&self, key: &[u8], value: &[u8]) -> fm_core::MemoryResult<()> {
        fs_core::FusionStoreEngine::put_kv(&self.engine, key, value, None)
            .map_err(Self::map_err)
            .map_err(StoreError::to_memory)?;
        Ok(())
    }

    fn get_kv_zero_copy(&self, key: &[u8]) -> fm_core::MemoryResult<Option<ZeroCopyBuffer>> {
        let res = fs_core::FusionStoreEngine::get_kv_zero_copy(&self.engine, key, None)
            .map_err(Self::map_err)
            .map_err(StoreError::to_memory)?;
        // fs-core ZeroCopyBuffer (mmap 段) → fm-store ZeroCopyBuffer (owned)。拷出脱离 mmap 生命周期。
        Ok(res.map(|zcb| ZeroCopyBuffer::new(zcb.as_bytes().to_vec())))
    }

    fn insert_vector(&self, id: u64, vec: &[f32]) -> fm_core::MemoryResult<()> {
        self.dim_check(vec).map_err(StoreError::to_memory)?;
        fs_core::FusionStoreEngine::insert_vector(&self.engine, id, vec, None)
            .map_err(Self::map_err)
            .map_err(StoreError::to_memory)?;
        Ok(())
    }

    fn search_knn(&self, query: &[f32], top_k: usize) -> fm_core::MemoryResult<Vec<(u64, f32)>> {
        self.dim_check(query).map_err(StoreError::to_memory)?;
        let raw = fs_core::FusionStoreEngine::search_knn(&self.engine, query, top_k, None)
            .map_err(Self::map_err)
            .map_err(StoreError::to_memory)?;
        // fs-core 返 distance (1-cos_sim); fusion-memory 契约返 similarity。转 1.0 - d。
        // 同 local.rs:319 公式 (DistCosine.eval = 1-cos_sim, similarity = 1 - distance)。
        let out = raw
            .into_iter()
            .map(|(id, distance)| (id, 1.0 - distance))
            .collect();
        Ok(out)
    }

    fn get_vector(&self, id: u64) -> fm_core::MemoryResult<Option<Vec<f32>>> {
        let res = fs_core::FusionStoreEngine::get_vector(&self.engine, id, None)
            .map_err(Self::map_err)
            .map_err(StoreError::to_memory)?;
        Ok(res)
    }

    fn delete_vector(&self, id: u64) -> fm_core::MemoryResult<()> {
        // fs-core delete_vector 返 bool (是否删到); fusion-memory 契约返 () (删不到不报错, 幂等)。
        let _ = fs_core::FusionStoreEngine::delete_vector(&self.engine, id, None)
            .map_err(Self::map_err)
            .map_err(StoreError::to_memory)?;
        Ok(())
    }

    fn list_vector_ids(&self) -> fm_core::MemoryResult<Vec<u64>> {
        let ids = fs_core::FusionStoreEngine::list_vector_ids(&self.engine, None)
            .map_err(Self::map_err)
            .map_err(StoreError::to_memory)?;
        Ok(ids)
    }

    fn dimension(&self) -> usize {
        self.dim
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_store(dim: usize) -> FusionStore {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir =
            std::env::temp_dir().join(format!("fm-store-fusion-test-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        FusionStore::open(&dir, dim).unwrap()
    }

    #[test]
    fn kv_roundtrip() {
        let s = tmp_store(4);
        s.put_kv(b"k1", b"v1").unwrap();
        let got = s.get_kv_zero_copy(b"k1").unwrap().unwrap();
        assert_eq!(got.as_bytes(), b"v1");
        let miss = s.get_kv_zero_copy(b"missing").unwrap();
        assert!(miss.is_none());
    }

    #[test]
    fn vector_insert_get_search() {
        let s = tmp_store(3);
        let v0 = vec![1.0, 0.0, 0.0];
        let v1 = vec![0.9, 0.1, 0.0];
        let v2 = vec![0.0, 1.0, 0.0];
        s.insert_vector(10, &v0).unwrap();
        s.insert_vector(11, &v1).unwrap();
        s.insert_vector(12, &v2).unwrap();
        assert_eq!(s.get_vector(10).unwrap(), Some(v0.clone()));
        let hits = s.search_knn(&v0, 2).unwrap();
        assert_eq!(hits.len(), 2);
        // similarity (1.0 - distance): 自身相似度应最高 ≈ 1.0
        assert_eq!(hits[0].0, 10);
        assert!(
            hits[0].1 > 0.99,
            "self similarity > 0.99, got {}",
            hits[0].1
        );
        assert_eq!(hits[1].0, 11);
    }

    #[test]
    fn dimension_mismatch_rejected() {
        let s = tmp_store(4);
        let err = s.insert_vector(1, &[1.0, 0.0]).unwrap_err();
        assert!(matches!(err, fm_core::MemoryError::Store(_)));
    }

    #[test]
    fn search_knn_dimension_mismatch() {
        let s = tmp_store(3);
        let err = s.search_knn(&[1.0, 0.0], 2).unwrap_err();
        assert!(matches!(err, fm_core::MemoryError::Store(_)));
    }

    #[test]
    fn delete_then_get_none() {
        let s = tmp_store(2);
        s.insert_vector(20, &[1.0, 0.0]).unwrap();
        assert!(s.get_vector(20).unwrap().is_some());
        s.delete_vector(20).unwrap();
        // fs-core delete 软删 → get_vector 返 None (排除软删)
        assert_eq!(s.get_vector(20).unwrap(), None);
    }

    #[test]
    fn list_vector_ids_excludes_deleted() {
        let s = tmp_store(2);
        s.insert_vector(30, &[1.0, 0.0]).unwrap();
        s.insert_vector(31, &[0.0, 1.0]).unwrap();
        s.delete_vector(30).unwrap();
        let ids = s.list_vector_ids().unwrap();
        assert!(ids.contains(&31), "live id 31 present, got {ids:?}");
        assert!(!ids.contains(&30), "deleted id 30 excluded, got {ids:?}");
    }
}
