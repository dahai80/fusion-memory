//! issue #33 (v1.5.0): browse/manage DTOs for the web surface.
//!
//! Owned by fm-core so the engine trait can expose browse/manage RPCs without
//! depending on fm-persist (fm-persist owns its own richer row types; the
//! engine maps them onto these DTOs at the trait boundary).

use serde::{Deserialize, Serialize};

/// Parameters for `list_memories` (browse + paged read over the store).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListQuery {
    /// Row offset into the (filtered, created_timestamp ASC) result set.
    #[serde(default)]
    pub offset: usize,
    /// Page size. RPC layer caps this (hard max 200) to bound response size.
    #[serde(default = "default_limit")]
    pub limit: usize,
    /// Optional memory_type filter (Episodic/Semantic/Procedural).
    #[serde(default)]
    pub memory_type: Option<String>,
    /// issue #21 namespace visibility filter (user/project/local, empty = no filter).
    #[serde(default)]
    pub namespace: String,
    /// issue #21 namespace key (project path / machine id / user id).
    #[serde(default)]
    pub namespace_key: String,
    /// Include tombstoned (soft-deleted) rows.
    #[serde(default)]
    pub include_tombstoned: bool,
}

fn default_limit() -> usize {
    50
}

impl Default for ListQuery {
    fn default() -> Self {
        Self {
            offset: 0,
            limit: default_limit(),
            memory_type: None,
            namespace: String::new(),
            namespace_key: String::new(),
            include_tombstoned: false,
        }
    }
}

/// One audit_log row (browse DTO; mirrors fm-persist AuditLogEntry).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    pub id: u64,
    pub at: u64,
    pub actor: String,
    pub action: String,
    pub target_id: String,
    pub detail: String,
}

/// One merge_log row (browse DTO; mirrors fm-persist MergeLogEntry).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MergeEntry {
    pub id: u64,
    pub at: u64,
    pub source_id: String,
    pub target_id: String,
    pub reason: String,
}
