//! 记忆条目 + 记忆类型/层级。PRD §5.1, §5.2。
//!
//! 存储最小单元 = 单轮 turn（用户裁定）。一条 turn = 一条 MemoryItem。

use serde::{Deserialize, Serialize};

use crate::entity::EntityNode;

/// 记忆类型：事件性/语义性/程序性。各自衰减时间常数 τ 不同（PRD §5.1）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MemoryType {
    Episodic,
    Semantic,
    Procedural,
}

impl MemoryType {
    pub fn as_str(self) -> &'static str {
        match self {
            MemoryType::Episodic => "Episodic",
            MemoryType::Semantic => "Semantic",
            MemoryType::Procedural => "Procedural",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "Episodic" => Some(Self::Episodic),
            "Semantic" => Some(Self::Semantic),
            "Procedural" => Some(Self::Procedural),
            _ => None,
        }
    }

    /// 初始权重 W0（PRD §5.1）。
    pub fn initial_weight(self) -> f64 {
        match self {
            MemoryType::Episodic => 0.6,
            MemoryType::Semantic => 0.8,
            MemoryType::Procedural => 1.0,
        }
    }

    /// 衰减时间常数 τ（秒）。Episodic=1天 / Semantic=30天 / Procedural=90天。
    pub fn tau_seconds(self) -> f64 {
        match self {
            MemoryType::Episodic => 86_400.0,
            MemoryType::Semantic => 2_592_000.0,
            MemoryType::Procedural => 7_776_000.0,
        }
    }
}

/// 三级记忆调度层级。PRD §6。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MemoryTier {
    Working,
    Short,
    Long,
}

impl MemoryTier {
    pub fn as_str(self) -> &'static str {
        match self {
            MemoryTier::Working => "working",
            MemoryTier::Short => "short",
            MemoryTier::Long => "long",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "working" => Some(Self::Working),
            "short" => Some(Self::Short),
            "long" => Some(Self::Long),
            _ => None,
        }
    }
}

/// #57 safety metadata: 可见范围。写入即可撤销/可围栏 (arXiv 2610.00093)。
/// session=仅本会话 / agent=仅本 agent 命名空间 / team=团队共享 / global=全局。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum SafetyScope {
    #[default]
    Session,
    Agent,
    Team,
    Global,
}

impl SafetyScope {
    pub fn as_str(self) -> &'static str {
        match self {
            SafetyScope::Session => "session",
            SafetyScope::Agent => "agent",
            SafetyScope::Team => "team",
            SafetyScope::Global => "global",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "session" => Some(Self::Session),
            "agent" => Some(Self::Agent),
            "team" => Some(Self::Team),
            "global" => Some(Self::Global),
            _ => None,
        }
    }
}

/// #57 safety metadata: 权威层级 — 驱动衰减速度 (低权威更快遗忘) 与准入阈值。
/// self=本 agent 自身经验 / observed=观察到的他方行为 / team=团队沉淀 / system=系统写入 (恒准入)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum SafetyAuthority {
    #[default]
    #[serde(rename = "self")]
    SelfAuth,
    Observed,
    Team,
    System,
}

impl SafetyAuthority {
    pub fn as_str(self) -> &'static str {
        match self {
            SafetyAuthority::SelfAuth => "self",
            SafetyAuthority::Observed => "observed",
            SafetyAuthority::Team => "team",
            SafetyAuthority::System => "system",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "self" => Some(Self::SelfAuth),
            "observed" => Some(Self::Observed),
            "team" => Some(Self::Team),
            "system" => Some(Self::System),
            _ => None,
        }
    }
}

/// #57 safety metadata: 保留类别。
/// ephemeral=短住 (快速衰减) / normal=常规遗忘曲线 / pinned=钉住 (不参与回收)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum SafetyPersistence {
    Ephemeral,
    #[default]
    Normal,
    Pinned,
}

impl SafetyPersistence {
    pub fn as_str(self) -> &'static str {
        match self {
            SafetyPersistence::Ephemeral => "ephemeral",
            SafetyPersistence::Normal => "normal",
            SafetyPersistence::Pinned => "pinned",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "ephemeral" => Some(Self::Ephemeral),
            "normal" => Some(Self::Normal),
            "pinned" => Some(Self::Pinned),
            _ => None,
        }
    }
}

/// #57 准入门裁决记录 — 与记忆同行落库供审计 (accepted/rejected + score + reason)。
/// 确定性门 (Rule 5): 无模型调用, 判定可离线重放。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdmissionRecord {
    pub accepted: bool,
    pub score: f32,
    pub reason: String,
    pub gate_version: u32,
}

/// 记忆条目（turn 级最小存储单元）。PRD §5.2 刷新版。
///
/// 关键字段（相对原始 PRD 变更见 PRD §5.2 注释）：
/// - `interaction_id` + `turn_idx`: turn 级存储，聚合检索。
/// - `vector_ref: String`: 统一 ID 体系（C6 修正）。
/// - `weight: f64`: 抗精度损失（B3 修正）。
/// - `tombstone: bool`: 跨库一致删除标记（A1 修正）。
/// - `entities_pending: bool`: 抽取失败待补抽（C5 修正）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryItem {
    pub id: String,
    pub interaction_id: String,
    pub turn_idx: u32,
    pub session_id: String,
    /// #16 多租户隔离: 权威租户标识 (来自 gateway X-Fusion-Tenant)。
    /// 空 = 默认租户 (单租户向后兼容, 旧库迁移后 tenant 列 DEFAULT '')。
    pub tenant: String,
    /// issue #21 作用域命名空间: user/project/local, 空 = 全局 (旧行为)。
    #[serde(default)]
    pub namespace: String,
    /// issue #21 命名空间键 (project path / machine id / user id)。空 = 同级不限 key。
    #[serde(default)]
    pub namespace_key: String,
    pub memory_type: MemoryType,
    pub tier: MemoryTier,
    pub content: String,
    pub entities: Vec<EntityNode>,
    pub vector_ref: String,
    pub weight: f64,
    pub access_count: u64,
    pub last_accessed_timestamp: u64,
    pub created_timestamp: u64,
    pub provenance: Option<String>,
    pub tombstone: bool,
    pub entities_pending: bool,
    /// M3 #44 事实有效期窗口: 生效起点 (epoch ms)。None = 无下界。
    /// 旧数据无窗口 → 检索不受有效期影响 (serde default + 列可空)。
    #[serde(default)]
    pub valid_from: Option<u64>,
    /// M3 #44 事实有效期窗口: 失效终点 (epoch ms)。None = 无上界。
    /// 过期记忆降权不删除 — tombstone 仍只给显式删除。
    #[serde(default)]
    pub valid_until: Option<u64>,
    /// M6 #45 信任链: 巩固产物置信度 (TRUSTMEM 传闻→置信度)。
    /// None = 非巩固产物 (commit 直写)。0..1。
    #[serde(default)]
    pub confidence: Option<f32>,
    /// M6 #45 溯源: 来源引用 (episodes)。
    /// summarize 产物 = 来源 interaction ids; merge 产物 = 被并 item ids。空 = 无溯源。
    #[serde(default)]
    pub sources: Vec<String>,
    /// M5 #46 per-memory ACL: owner user id (fusion-identity JWT sub)。
    /// None = 共享/旧数据 (所有租户内调用方可见)。仅 multi_tenant+ACL 模式下由服务端权威写入。
    #[serde(default)]
    pub owner: Option<String>,
    /// #57 safety metadata: 可见范围 (旧数据 = Session 默认)。
    #[serde(default)]
    pub scope: SafetyScope,
    /// #57 safety metadata: 权威层级 (低权威衰减更快, 遗忘曲线 × authority 因子)。
    #[serde(default)]
    pub authority: SafetyAuthority,
    /// #57 safety metadata: 保留类别 (pinned 不参与回收)。
    #[serde(default)]
    pub persistence: SafetyPersistence,
    /// #57 准入门裁决 (None = 门未启用 / 门前数据)。
    #[serde(default)]
    pub admission: Option<AdmissionRecord>,
}

impl MemoryItem {
    /// 构造 turn 级骨架（commit 同步快路径用，PRD §6.3）。
    /// vector_ref 置空，entities_pending=true，待异步回填。
    #[allow(clippy::too_many_arguments)]
    pub fn new_turn_skeleton(
        id: String,
        interaction_id: String,
        turn_idx: u32,
        session_id: String,
        tenant: String,
        memory_type: MemoryType,
        content: String,
        created_timestamp: u64,
    ) -> Self {
        Self {
            id,
            interaction_id,
            turn_idx,
            session_id,
            tenant,
            namespace: String::new(),
            namespace_key: String::new(),
            memory_type,
            tier: MemoryTier::Working,
            content,
            entities: Vec::new(),
            vector_ref: String::new(),
            weight: memory_type.initial_weight(),
            access_count: 0,
            last_accessed_timestamp: created_timestamp,
            created_timestamp,
            provenance: None,
            tombstone: false,
            entities_pending: true,
            valid_from: None,
            valid_until: None,
            confidence: None,
            sources: Vec::new(),
            owner: None,
            scope: SafetyScope::Session,
            authority: SafetyAuthority::SelfAuth,
            persistence: SafetyPersistence::Normal,
            admission: None,
        }
    }

    /// #57: 挂 safety metadata (scope/authority/persistence)。
    pub fn with_safety(
        mut self,
        scope: SafetyScope,
        authority: SafetyAuthority,
        persistence: SafetyPersistence,
    ) -> Self {
        self.scope = scope;
        self.authority = authority;
        self.persistence = persistence;
        self
    }

    /// #57: 挂准入门裁决记录。
    pub fn with_admission(mut self, record: AdmissionRecord) -> Self {
        self.admission = Some(record);
        self
    }

    /// issue #21: 挂作用域命名空间 (空 namespace = 全局, 不改旧调用点签名)。
    pub fn with_namespace(mut self, namespace: impl Into<String>, key: impl Into<String>) -> Self {
        self.namespace = namespace.into();
        self.namespace_key = key.into();
        self
    }

    /// M3 #44: 挂事实有效期窗口 (None = 不设界)。
    pub fn with_validity(mut self, valid_from: Option<u64>, valid_until: Option<u64>) -> Self {
        self.valid_from = valid_from;
        self.valid_until = valid_until;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_type_weights() {
        assert!((MemoryType::Episodic.initial_weight() - 0.6).abs() < 1e-9);
        assert!((MemoryType::Semantic.initial_weight() - 0.8).abs() < 1e-9);
        assert!((MemoryType::Procedural.initial_weight() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn memory_type_tau_ordering() {
        assert!(MemoryType::Episodic.tau_seconds() < MemoryType::Semantic.tau_seconds());
        assert!(MemoryType::Semantic.tau_seconds() < MemoryType::Procedural.tau_seconds());
    }

    #[test]
    fn tier_roundtrip() {
        for t in [MemoryTier::Working, MemoryTier::Short, MemoryTier::Long] {
            assert_eq!(MemoryTier::parse(t.as_str()), Some(t));
        }
        assert_eq!(MemoryTier::parse("bogus"), None);
    }

    #[test]
    fn skeleton_defaults() {
        let m = MemoryItem::new_turn_skeleton(
            "id".into(),
            "ix".into(),
            0,
            "s".into(),
            String::new(),
            MemoryType::Semantic,
            "hi".into(),
            1000,
        );
        assert_eq!(m.tier, MemoryTier::Working);
        assert!(m.entities_pending);
        assert!(m.vector_ref.is_empty());
        assert!(!m.tombstone);
        assert!(m.entities.is_empty());
        assert!((m.weight - 0.8).abs() < 1e-9);
    }
}
