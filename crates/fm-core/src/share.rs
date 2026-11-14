//! #58: agent 间共享授权 + #57 元数据撤销过滤器。
//!
//! ShareGrant = grantor_key 授权 grantee_key 读自己 agent 命名空间 (namespace='agent')。
//! 显式授权才可见 (默认隔离, 防经验互相污染)。
//! RevokeFilter = revoke RPC 的元数据匹配器 (按 scope/authority/persistence/session 撤销,
//! 不止按 id) — 所有字段 None = 空过滤器 (服务端拒绝空过滤, 防误全删)。

use serde::{Deserialize, Serialize};

/// #58: 一条 agent 命名空间读授权。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShareGrant {
    /// 被授权读谁的数据 (grantor 的 agent namespace_key)。
    pub grantor_key: String,
    /// 授权给谁 (grantee 的 agent namespace_key)。
    pub grantee_key: String,
    /// 授权限定 namespace 层级 (现恒 "agent"; 预留层级扩展)。
    pub namespace: String,
    /// 授权时间 (epoch ms)。
    pub created_at: u64,
}

/// #57: revoke-by-metadata 过滤器。全部字段 = AND 语义; 全空 = 服务端拒绝。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevokeFilter {
    pub scope: Option<String>,
    pub authority: Option<String>,
    pub persistence: Option<String>,
    pub session_id: Option<String>,
    pub namespace: Option<String>,
}

impl RevokeFilter {
    pub fn is_empty(&self) -> bool {
        self.scope.is_none()
            && self.authority.is_none()
            && self.persistence.is_none()
            && self.session_id.is_none()
            && self.namespace.is_none()
    }

    /// 校验枚举字段值合法 (fail-closed, 防 typo 撤销不到却报成功)。
    pub fn validate(&self) -> Result<(), String> {
        if self.is_empty() {
            return Err(
                "revoke filter is empty — refuse to revoke everything; set at least one field"
                    .into(),
            );
        }
        if let Some(s) = &self.scope {
            crate::memory::SafetyScope::parse(s)
                .ok_or_else(|| format!("unknown scope {s:?} (session|agent|team|global)"))?;
        }
        if let Some(a) = &self.authority {
            crate::memory::SafetyAuthority::parse(a)
                .ok_or_else(|| format!("unknown authority {a:?} (self|observed|team|system)"))?;
        }
        if let Some(p) = &self.persistence {
            crate::memory::SafetyPersistence::parse(p)
                .ok_or_else(|| format!("unknown persistence {p:?} (ephemeral|normal|pinned)"))?;
        }
        Ok(())
    }
}
