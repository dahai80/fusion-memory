//! 向量存储引擎 trait。PRD §8.1 补齐版。
//!
//! A4 修正: get_vector 返回 owned Vec<f32>（放弃零拷贝幻象，1024×f32=4KB 拷贝可忽略）。
//! A1/C3 修正: delete_vector 软删/物理删向量。

use fm_core::MemoryResult;

/// 零拷贝缓冲（store-fusion 下为 mmap 切片；store-stub 不用）。
/// M1 保留类型签名，实际 store-stub 走 owned。
pub struct ZeroCopyBuffer {
    pub data: Vec<u8>,
}

impl ZeroCopyBuffer {
    pub fn new(data: Vec<u8>) -> Self {
        Self { data }
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.data
    }
}

/// 向量存储引擎 trait。PRD §8.1。
pub trait FusionStoreEngine: Send + Sync {
    /// KV 原始写入。
    fn put_kv(&self, key: &[u8], value: &[u8]) -> MemoryResult<()>;

    /// KV 零拷贝读。
    fn get_kv_zero_copy(&self, key: &[u8]) -> MemoryResult<Option<ZeroCopyBuffer>>;

    /// 插入向量（id = ulid hash → u64）。
    fn insert_vector(&self, id: u64, vec: &[f32]) -> MemoryResult<()>;

    /// KNN 查询，返回 (vector_id, similarity)。
    fn search_knn(&self, query: &[f32], top_k: usize) -> MemoryResult<Vec<(u64, f32)>>;

    /// 取向量本身（A4 修正: owned）。
    fn get_vector(&self, id: u64) -> MemoryResult<Option<Vec<f32>>>;

    /// 删向量（A1/C3 修正: tombstone 软删，compact 物理删）。
    fn delete_vector(&self, id: u64) -> MemoryResult<()>;

    /// 维度。
    fn dimension(&self) -> usize;
}
