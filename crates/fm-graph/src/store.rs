//! §1.5: 图层存储抽象 trait。解耦 fm-graph 与具体 fm_persist::Persist。
//!
//! fm-graph 生产路径仅用 Persist 的 2 个读方法: `n_hop_reachable` (图遍历) +
//! `list_entities_by_type` (对齐候选枚举)。抽成 `GraphStore` trait 后, 图层可注入
//! 内存 mock 单测 (审计 §1.5: "任何想用内存图单测 graph_affinity/align_entity 的测试
//! 必须起真实 Persist::open_in_memory() + SQL 填数据")。
//!
//! trait 住 fm-graph (消费方), impl 住本文件 (fm-persist 的适配层), 反转依赖: fm-graph
//! 不再硬编码 `use fm_persist::Persist`, 而是接收 `&dyn GraphStore`。

use fm_persist::Persist;

use crate::error::{GraphError, GraphResult};

/// 图层所需的最小存储接口 (§1.5: n_hop_reachable + list_entities_by_type)。
/// 其余 Persist 方法 (put_memory/put_relation/put_wop/audit) 不属图层职责,
/// 不进 trait — 审计 §1.5: Persist god-object 30+ 方法跨 5 职责一把锁。
pub trait GraphStore {
    /// N-hop 可达节点 (start 出发 hop_limit 跳内可达的 entity id, 最短跳)。
    fn n_hop_reachable(&self, start: &str, hop_limit: usize) -> GraphResult<Vec<(String, usize)>>;

    /// 按 entity_type 查全部实体 (id, name, aliases_json)。
    fn list_entities_by_type(
        &self,
        entity_type: &str,
    ) -> GraphResult<Vec<(String, String, String)>>;
}

// §1.5: Persist 已有这两个方法 (返 PersistResult), impl trait 把 PersistError → GraphError。
// GraphError 已有 #[from] PersistError, 直接 `?` 转换。
impl GraphStore for Persist {
    fn n_hop_reachable(&self, start: &str, hop_limit: usize) -> GraphResult<Vec<(String, usize)>> {
        Persist::n_hop_reachable(self, start, hop_limit).map_err(GraphError::from)
    }

    fn list_entities_by_type(
        &self,
        entity_type: &str,
    ) -> GraphResult<Vec<(String, String, String)>> {
        Persist::list_entities_by_type(self, entity_type).map_err(GraphError::from)
    }
}
