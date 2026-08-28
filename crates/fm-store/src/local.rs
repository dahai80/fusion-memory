//! local-store 长期生产后端。PRD §8.2/§8.4。
//!
//! hnsw_rs (内存 HNSW 索引) + sled (KV + 向量持久化 + tombstone)。
//! 崩溃恢复: 启动从 sled `vec` tree 重放重建 hnsw，跳过 tombstone。
//! 软删: delete_vector 写 tomb tree; compact 物理删 + 重建索引。

use std::path::Path;
use std::sync::RwLock;

use hnsw_rs::prelude::*;
use sled::Db;
use tracing::{debug, info, warn};

use crate::error::{StoreError, StoreResult};
use crate::trait_def::{FusionStoreEngine, ZeroCopyBuffer};

const TREE_KV: &str = "kv";
const TREE_VEC: &str = "vec";
const TREE_TOMB: &str = "tomb";

const HNSW_M: usize = 16;
const HNSW_MAX_LAYER: usize = 6;
const HNSW_EF_CONSTRUCT: usize = 200;
const SEARCH_EF: usize = 64;

/// 手写余弦相似度 (search_knn 补齐用, 避免引入 fm-similarity 依赖)。返回 [-1, 1]。
fn cosine_sim(a: &[f32], b: &[f32]) -> f32 {
    let mut dot = 0.0f32;
    let mut na = 0.0f32;
    let mut nb = 0.0f32;
    for (x, y) in a.iter().zip(b.iter()) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    let denom = na.sqrt() * nb.sqrt();
    if denom == 0.0 {
        0.0
    } else {
        dot / denom
    }
}

pub struct LocalStore {
    db: Db,
    // §3.11: 旧版每次 vec_tree()/tomb_tree()/kv_tree() 都 db.open_tree → 每调用一次 sled 路径解析 + tree 句柄分配。
    // search_knn 单次至少 open vec+tomb 两次; 高 QPS 下成倍放大 open_tree 开销。
    // 改: open 时一次性打开三棵 tree 缓存, 后续直接复用 (sled::Tree 是 Arc handle, clone 廉价)。
    vec_tree: sled::Tree,
    tomb_tree: sled::Tree,
    kv_tree: sled::Tree,
    dim: usize,
    hnsw: RwLock<Hnsw<'static, f32, DistCosine>>,
}

impl LocalStore {
    pub fn open(path: impl AsRef<Path>, dim: usize) -> StoreResult<Self> {
        if dim == 0 {
            return Err(StoreError::Dimension {
                expected: 1,
                got: 0,
            });
        }
        let db = sled::Config::new()
            .path(path)
            // §2.13: 旧版 flush_every_ms(Some(1_000)) 每 1s 批刷; 进程突崩丢 ≤1s 写。
            // 配合 Drop flush (§2.13 下半) 保优雅退出落盘; 默认值不变, 仅补 Drop 兜底。
            .flush_every_ms(Some(1_000))
            .open()
            .map_err(|e| StoreError::Sled(e.to_string()))?;
        let vec_tree = db
            .open_tree(TREE_VEC)
            .map_err(|e| StoreError::Sled(e.to_string()))?;
        let tomb_tree = db
            .open_tree(TREE_TOMB)
            .map_err(|e| StoreError::Sled(e.to_string()))?;
        let kv_tree = db
            .open_tree(TREE_KV)
            .map_err(|e| StoreError::Sled(e.to_string()))?;
        let hnsw = Hnsw::new(HNSW_M, 1024, HNSW_MAX_LAYER, HNSW_EF_CONSTRUCT, DistCosine);
        let stub = Self {
            db,
            vec_tree,
            tomb_tree,
            kv_tree,
            dim,
            hnsw: RwLock::new(hnsw),
        };
        stub.rebuild_from_sled()?;
        info!(dim = stub.dim, "local-store opened");
        Ok(stub)
    }

    pub fn open_temp(dim: usize) -> StoreResult<Self> {
        let dir = std::env::temp_dir().join(format!("fm-local-store-{}", std::process::id()));
        Self::open(&dir, dim)
    }

    fn is_tombstoned(&self, id: u64) -> StoreResult<bool> {
        let exists = self
            .tomb_tree
            .contains_key(id.to_be_bytes())
            .map_err(|e| StoreError::Sled(e.to_string()))?;
        Ok(exists)
    }

    // §3.3: 解析 8 字节大端 u64 key。坏 key (非 8 字节) 不再 unwrap_or([0u8;8]) 静默归零 → id0 碰撞,
    // 改跳过并 warn (返回 None); 调用方 filter_map 跳过坏行, 不污染 id0 也不 panic。
    fn parse_id_key(key: &[u8]) -> Option<u64> {
        if key.len() != 8 {
            warn!(
                len = key.len(),
                "store key not 8 bytes, skip (would corrupt id0)"
            );
            return None;
        }
        let arr: [u8; 8] = key.try_into().expect("len==8 checked");
        Some(u64::from_be_bytes(arr))
    }

    // P4: 一次性加载全部 tombstone id 入 HashSet, 供 search_knn 批量过滤。
    // 旧版对每个邻居/每个 fallback 向量逐个 is_tombstoned 单点查 sled → N 次 I/O/搜索。
    /// 单次 tree.iter 扫描替代 N 次点查; tomb 集合通常远小于全量, 内存可控。
    fn tombstone_set(&self) -> StoreResult<std::collections::HashSet<u64>> {
        let mut out = std::collections::HashSet::new();
        for item in self.tomb_tree.iter() {
            let (k, _v) = item.map_err(|e| StoreError::Sled(e.to_string()))?;
            if let Some(id) = Self::parse_id_key(&k) {
                out.insert(id);
            }
        }
        Ok(out)
    }

    /// 枚举所有非 tombstone 向量 id (L3 反向对账: store→SQLite 孤儿扫描)。
    /// 跳过 tomb tree 标记的软删向量。用于 reconcile 检测 store 有向量但 SQLite 无元数据的孤儿。
    pub fn list_vector_ids(&self) -> StoreResult<Vec<u64>> {
        let mut out = Vec::new();
        for item in self.vec_tree.iter() {
            let (key, _val) = item.map_err(|e| StoreError::Sled(e.to_string()))?;
            let Some(id) = Self::parse_id_key(&key) else {
                continue;
            };
            if self
                .tomb_tree
                .contains_key(&key)
                .map_err(|e| StoreError::Sled(e.to_string()))?
            {
                continue;
            }
            out.push(id);
        }
        Ok(out)
    }

    /// 向量序列化: 紧凑 LE f32 原始字节 (P2 修正, 替 serde_json 文本编码)。
    /// serde_json 编码 `[1.0,0.0,...]` 约 7-12B/float; 原始字节 4B/float, ~3x 省 + 解析零分配。
    /// 无版本前缀: dim 在调用点校验, 长度 / 4 = 元素数, 自描述。
    fn serialize_vec(vec: &[f32]) -> StoreResult<Vec<u8>> {
        let mut bytes = Vec::with_capacity(vec.len() * 4);
        for &f in vec {
            bytes.extend_from_slice(&f.to_le_bytes());
        }
        Ok(bytes)
    }

    fn deserialize_vec(bytes: &[u8]) -> StoreResult<Vec<f32>> {
        if !bytes.len().is_multiple_of(4) {
            return Err(StoreError::Sled(format!(
                "vec bytes len {} not multiple of 4 (corrupt or legacy json format)",
                bytes.len()
            )));
        }
        let mut out = Vec::with_capacity(bytes.len() / 4);
        for chunk in bytes.chunks_exact(4) {
            let arr: [u8; 4] = chunk.try_into().expect("chunks_exact(4) → [u8;4]");
            out.push(f32::from_le_bytes(arr));
        }
        Ok(out)
    }

    /// 从 sled vec tree 重放重建 hnsw 索引（崩溃恢复 / 启动加载）。
    pub fn rebuild_from_sled(&self) -> StoreResult<usize> {
        let mut loaded = 0usize;
        let new_hnsw = Hnsw::new(HNSW_M, 1024, HNSW_MAX_LAYER, HNSW_EF_CONSTRUCT, DistCosine);
        for item in self.vec_tree.iter() {
            let (key, val) = item.map_err(|e| StoreError::Sled(e.to_string()))?;
            // §3.3: 坏 key 不再 unwrap_or([0u8;8]) 归零 (→ 全部碰撞到真实 id0 的向量, 索引污染)。
            let Some(id) = Self::parse_id_key(&key) else {
                continue;
            };
            if self.is_tombstoned(id)? {
                debug!(id, "skip tombstoned on rebuild");
                continue;
            }
            let vec = Self::deserialize_vec(val.as_ref())?;
            new_hnsw.insert((&vec, id as usize));
            loaded += 1;
        }
        let mut guard = self
            .hnsw
            .write()
            .map_err(|e| StoreError::Hnsw(format!("hnsw lock poisoned: {e}")))?;
        *guard = new_hnsw;
        if loaded > 0 {
            info!(loaded, "rebuild hnsw from sled");
        } else {
            debug!("rebuild hnsw: empty store");
        }
        Ok(loaded)
    }

    /// 物理删所有 tombstoned 向量并重建索引（compact）。
    pub fn compact(&self) -> StoreResult<usize> {
        let mut removed = 0usize;
        // §3.3: 坏 tomb key 不再 unwrap_or([0u8;8]) 归零 (→ 误删 id0 真实向量)。
        let tomb_keys: Vec<u64> = self
            .tomb_tree
            .iter()
            .filter_map(|item| item.ok().and_then(|(k, _)| Self::parse_id_key(&k)))
            .collect();
        for id in &tomb_keys {
            self.vec_tree
                .remove(id.to_be_bytes())
                .map_err(|e| StoreError::Sled(e.to_string()))?;
            removed += 1;
        }
        if removed > 0 {
            info!(removed, "compact: removed tombstoned vectors");
        }
        self.rebuild_from_sled()?;
        Ok(removed)
    }

    pub fn flush(&self) -> StoreResult<()> {
        self.db
            .flush()
            .map_err(|e| StoreError::Sled(e.to_string()))?;
        Ok(())
    }
}

impl FusionStoreEngine for LocalStore {
    fn put_kv(&self, key: &[u8], value: &[u8]) -> fm_core::MemoryResult<()> {
        self.kv_tree
            .insert(key, value)
            .map_err(|e| StoreError::to_memory(StoreError::Sled(e.to_string())))?;
        Ok(())
    }

    fn get_kv_zero_copy(&self, key: &[u8]) -> fm_core::MemoryResult<Option<ZeroCopyBuffer>> {
        let res = self
            .kv_tree
            .get(key)
            .map_err(|e| StoreError::to_memory(StoreError::Sled(e.to_string())))?;
        // §3.16: local-store 非 mmap, sled::IVec 需 .as_ref().to_vec() 拷出 (IVec 生命周期绑 db)。
        // ZeroCopyBuffer 类型名源自 store-fusion mmap 蓝图; stub 下实为 owned, 已在 trait_def 注释标注。
        Ok(res.map(|ia| ZeroCopyBuffer::new(ia.as_ref().to_vec())))
    }

    fn insert_vector(&self, id: u64, vec: &[f32]) -> fm_core::MemoryResult<()> {
        if vec.len() != self.dim {
            return Err(StoreError::to_memory(StoreError::Dimension {
                expected: self.dim,
                got: vec.len(),
            }));
        }
        // H2 幂等: 同 id 已落盘且未 tombstone → 视为已存在, 跳过 hnsw.insert。
        // replay 重放同一条目 (leader 重发/follower 重启) 不重复入索引, 避免 hnsw 重复点。
        // tombstone 状态 → 清 tomb 后照常重插入 (复活路径)。
        let already_present = self
            .vec_tree
            .contains_key(id.to_be_bytes())
            .map_err(|e| StoreError::to_memory(StoreError::Sled(e.to_string())))?;
        if already_present && !self.is_tombstoned(id).map_err(StoreError::to_memory)? {
            debug!(
                id,
                "insert_vector: id already present, skip hnsw insert (idempotent)"
            );
            return Ok(());
        }
        if self.is_tombstoned(id).map_err(StoreError::to_memory)? {
            warn!(id, "insert_vector: id tombstoned, clearing tombstone");
            self.tomb_tree
                .remove(id.to_be_bytes())
                .map_err(|e| StoreError::to_memory(StoreError::Sled(e.to_string())))?;
        }
        let bytes = Self::serialize_vec(vec).map_err(StoreError::to_memory)?;
        self.vec_tree
            .insert(id.to_be_bytes(), bytes)
            .map_err(|e| StoreError::to_memory(StoreError::Sled(e.to_string())))?;
        let hnsw = self.hnsw.read().map_err(|e| {
            StoreError::to_memory(StoreError::Hnsw(format!("hnsw lock poisoned: {e}")))
        })?;
        hnsw.insert((vec, id as usize));
        Ok(())
    }

    fn search_knn(&self, query: &[f32], top_k: usize) -> fm_core::MemoryResult<Vec<(u64, f32)>> {
        if query.len() != self.dim {
            return Err(StoreError::to_memory(StoreError::Dimension {
                expected: self.dim,
                got: query.len(),
            }));
        }
        let hnsw = self.hnsw.read().map_err(|e| {
            StoreError::to_memory(StoreError::Hnsw(format!("hnsw lock poisoned: {e}")))
        })?;
        let neighbours = hnsw.search(query, top_k, SEARCH_EF);
        // P4: tombstone 批量加载一次, 替代每邻居/每 fallback 向量单点 sled 查。
        let tombs = self.tombstone_set().map_err(StoreError::to_memory)?;
        let mut out = Vec::with_capacity(neighbours.len());
        let mut seen: std::collections::HashSet<u64> = std::collections::HashSet::new();
        for nb in neighbours {
            let id = nb.d_id as u64;
            if tombs.contains(&id) {
                continue;
            }
            // DistCosine.eval 返回 1 - cos_sim; similarity = 1 - distance
            let similarity = 1.0 - nb.distance;
            out.push((id, similarity));
            seen.insert(id);
        }
        // hnsw 近似召回在小数据集/边界 ef 下可能返回 < top_k。补齐: 线性扫 vec_tree
        // 取未命中活向量, 按 cosine 补足 top_k。保正确召回完整性 (Rule 12 不静默少返)。
        if out.len() < top_k {
            let mut candidates: Vec<(u64, f32)> = Vec::new();
            for item in self.vec_tree.iter() {
                let (k, v) =
                    item.map_err(|e| StoreError::to_memory(StoreError::Sled(e.to_string())))?;
                // §3.3/§3.10: 坏 key 不再 unwrap_or([0u8;8]) 归零 (会污染 id0), 跳过坏行。
                let Some(id) = Self::parse_id_key(&k) else {
                    continue;
                };
                if seen.contains(&id) || tombs.contains(&id) {
                    continue;
                }
                let vec = Self::deserialize_vec(&v).map_err(StoreError::to_memory)?;
                let sim = cosine_sim(query, &vec);
                candidates.push((id, sim));
            }
            candidates.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            for (id, sim) in candidates.into_iter().take(top_k - out.len()) {
                out.push((id, sim));
            }
            // 整体按相似度降序, 补齐后顺序一致
            out.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        }
        Ok(out)
    }

    fn get_vector(&self, id: u64) -> fm_core::MemoryResult<Option<Vec<f32>>> {
        if self.is_tombstoned(id).map_err(StoreError::to_memory)? {
            return Ok(None);
        }
        let res = self
            .vec_tree
            .get(id.to_be_bytes())
            .map_err(|e| StoreError::to_memory(StoreError::Sled(e.to_string())))?;
        match res {
            None => Ok(None),
            Some(ia) => {
                let vec = Self::deserialize_vec(ia.as_ref()).map_err(StoreError::to_memory)?;
                Ok(Some(vec))
            }
        }
    }

    fn delete_vector(&self, id: u64) -> fm_core::MemoryResult<()> {
        self.tomb_tree
            .insert(id.to_be_bytes(), &[1u8])
            .map_err(|e| StoreError::to_memory(StoreError::Sled(e.to_string())))?;
        debug!(id, "delete_vector: tombstoned");
        Ok(())
    }

    // §1.4: trait 化 list_vector_ids, 引擎经 dyn FusionStoreEngine 调用, 不绑死 LocalStore。
    fn list_vector_ids(&self) -> fm_core::MemoryResult<Vec<u64>> {
        LocalStore::list_vector_ids(self).map_err(StoreError::to_memory)
    }

    fn dimension(&self) -> usize {
        self.dim
    }
}

// §2.13: 进程退出时 sled 异步刷盘可能丢尾写。Drop 显式 flush + flush trees 兜底优雅落盘。
// flush 失败只 warn 不 panic (析构中 panic 不安全); 1s 批刷窗口外的写已落 WAL, 崩溃可恢复。
impl Drop for LocalStore {
    fn drop(&mut self) {
        if let Err(e) = self.tomb_tree.flush() {
            warn!(error = %e, "Drop: tomb_tree flush failed");
        }
        if let Err(e) = self.vec_tree.flush() {
            warn!(error = %e, "Drop: vec_tree flush failed");
        }
        if let Err(e) = self.kv_tree.flush() {
            warn!(error = %e, "Drop: kv_tree flush failed");
        }
        if let Err(e) = self.db.flush() {
            warn!(error = %e, "Drop: db flush failed");
        }
        debug!("local-store dropped (flushed)");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_store(dim: usize) -> LocalStore {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("fm-store-test-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        LocalStore::open(&dir, dim).unwrap()
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
        assert_eq!(hits[0].0, 10);
        assert!(hits[0].1 > 0.99);
        assert_eq!(hits[1].0, 11);
    }

    #[test]
    fn delete_tombstone_then_compact() {
        let s = tmp_store(2);
        s.insert_vector(20, &[1.0, 0.0]).unwrap();
        s.insert_vector(21, &[0.0, 1.0]).unwrap();
        s.delete_vector(20).unwrap();
        // tombstoned: get & search exclude it
        assert_eq!(s.get_vector(20).unwrap(), None);
        let hits = s.search_knn(&[1.0, 0.0], 5).unwrap();
        assert!(hits.iter().all(|(id, _)| *id != 20));
        let removed = s.compact().unwrap();
        assert_eq!(removed, 1);
        assert!(s.get_vector(21).unwrap().is_some());
    }

    #[test]
    fn dimension_mismatch_rejected() {
        let s = tmp_store(4);
        let err = s.insert_vector(1, &[1.0, 0.0]).unwrap_err();
        assert!(matches!(err, fm_core::MemoryError::Store(_)));
    }

    #[test]
    fn rebuild_restores_vectors() {
        let dir = std::env::temp_dir().join(format!(
            "fm-store-rebuild-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        {
            let s = LocalStore::open(&dir, 3).unwrap();
            s.insert_vector(30, &[1.0, 0.0, 0.0]).unwrap();
            s.insert_vector(31, &[0.0, 1.0, 0.0]).unwrap();
            s.flush().unwrap();
        }
        let s2 = LocalStore::open(&dir, 3).unwrap();
        assert_eq!(s2.get_vector(30).unwrap(), Some(vec![1.0, 0.0, 0.0]));
        let hits = s2.search_knn(&[1.0, 0.0, 0.0], 1).unwrap();
        assert_eq!(hits[0].0, 30);
    }

    #[test]
    fn reopen_id_survives_tombstone() {
        let dir = std::env::temp_dir().join(format!(
            "fm-store-tomb-reopen-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        {
            let s = LocalStore::open(&dir, 2).unwrap();
            s.insert_vector(40, &[1.0, 0.0]).unwrap();
            s.delete_vector(40).unwrap();
            s.flush().unwrap();
        }
        let s2 = LocalStore::open(&dir, 2).unwrap();
        assert_eq!(s2.get_vector(40).unwrap(), None);
    }

    #[test]
    fn open_temp_works() {
        let s = LocalStore::open_temp(4).unwrap();
        assert_eq!(s.dimension(), 4);
        s.insert_vector(1, &[1.0, 0.0, 0.0, 0.0]).unwrap();
        assert!(s.get_vector(1).unwrap().is_some());
    }

    #[test]
    fn reinsert_after_delete_clears_tombstone() {
        let s = tmp_store(2);
        s.insert_vector(50, &[1.0, 0.0]).unwrap();
        s.delete_vector(50).unwrap();
        assert!(s.get_vector(50).unwrap().is_none());
        // reinsert clears tombstone, vector visible again
        s.insert_vector(50, &[0.0, 1.0]).unwrap();
        assert!(s.get_vector(50).unwrap().is_some());
    }

    #[test]
    fn search_knn_dimension_mismatch() {
        let s = tmp_store(3);
        let err = s.search_knn(&[1.0, 0.0], 2).unwrap_err();
        assert!(matches!(err, fm_core::MemoryError::Store(_)));
    }

    #[test]
    fn compact_zero_when_no_tombstones() {
        let s = tmp_store(2);
        s.insert_vector(60, &[1.0, 0.0]).unwrap();
        let removed = s.compact().unwrap();
        assert_eq!(removed, 0);
    }
}
