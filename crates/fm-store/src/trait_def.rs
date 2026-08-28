//! 向量存储引擎 trait。PRD §8.1 补齐版。
//!
//! A4 修正: get_vector 返回 owned Vec<f32>（放弃零拷贝幻象，1024×f32=4KB 拷贝可忽略）。
//! A1/C3 修正: delete_vector 软删/物理删向量。

use fm_core::MemoryResult;

/// 字节缓冲。
/// §3.16: 类型名 ZeroCopyBuffer 源自 store-fusion mmap 零拷贝蓝图; 当前后端 local-store 走 owned
/// (sled::IVec 生命周期绑 db, 必须拷出), 非真零拷贝。保留类型名以稳定 trait ABI, 文档标注此处 owned。
/// store-fusion 落地后, data 可换 mmap 切片实现真零拷贝。
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

    /// §1.4: 枚举所有非 tombstone 向量 id (reconcile 反向对账: store→SQLite 孤儿扫描)。
    /// trait 化后 store 后端可换 (local-store/store-fusion), 引擎不绑死具体类型。
    fn list_vector_ids(&self) -> MemoryResult<Vec<u64>>;

    /// 维度。
    fn dimension(&self) -> usize;
}
