//! #57: 经验准入门 — 确定性重放式裁决 (Rule 5: 无模型调用, 可离线重放)。
//!
//! 重放代理 (Anchor arXiv 2611.10382 的确定性近似): 候选记忆在持久化前,
//! 对照 (1) 全库近邻 (KNN top-1 cosine) 与 (2) 同 session 近期轨迹, 判定是否 no-op:
//! - 近重复 (sim >= dup_sim): 源动作的复述, 不改变经验分布 → 拒绝
//! - 会话冗余 (token Jaccard >= 0.9): 同 session 轨迹已含同等信息 → 拒绝
//! - 低权威 (observed) 提高新颖性阈值 — 成功与失败的动作都可能被错误强化,
//!   低权威观察需要更强证据才入库
//! - system 权威恒准入 (运维/系统写入不受门限)
//!
//! 裁决随记忆落库 (AdmissionRecord) 供审计; 低权威记忆经 scoring::authority_age_factor
//! 加速衰减 (遗忘曲线作为门的一部分)。

use fm_core::{SafetyAuthority, SafetyPersistence, SafetyScope};

pub const GATE_VERSION: u32 = 1;
/// 近重复阈值 (cosine)。默认 0.95 — 与 consolidate merge 阈值 (0.92) 区分层级。
pub const DEFAULT_DUP_SIM: f32 = 0.95;
/// 最低新颖性 (1 - max_sim)。低于此拒 "low-novelty"。
pub const DEFAULT_MIN_NOVELTY: f32 = 0.10;
/// 会话冗余 Jaccard 阈值。
pub const SESSION_REDUNDANT_JACCARD: f64 = 0.9;
/// 会话冗余检查回看的近期轨迹条数。
pub const SESSION_TRACE_WINDOW: usize = 20;

#[derive(Debug, Clone)]
pub struct AdmissionConfig {
    pub enabled: bool,
    pub dup_sim: f32,
    pub min_novelty: f32,
}

impl Default for AdmissionConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            dup_sim: DEFAULT_DUP_SIM,
            min_novelty: DEFAULT_MIN_NOVELTY,
        }
    }
}

/// 准入裁决 (accepted/score/reason) — 落 AdmissionRecord 前的中间态。
#[derive(Debug, Clone, PartialEq)]
pub struct AdmissionVerdict {
    pub accepted: bool,
    pub score: f32,
    pub reason: String,
}

/// interaction.metadata 自由字段抽取 (valid_from 同款模式, 零 wire 契约变更):
/// metadata.scope / metadata.authority / metadata.persistence, 非法值降级默认 + warn 由调用方记。
pub fn safety_from_metadata(
    metadata: &serde_json::Value,
) -> (SafetyScope, SafetyAuthority, SafetyPersistence) {
    let scope = metadata
        .get("scope")
        .and_then(|v| v.as_str())
        .and_then(SafetyScope::parse);
    let authority = metadata
        .get("authority")
        .and_then(|v| v.as_str())
        .and_then(SafetyAuthority::parse);
    let persistence = metadata
        .get("persistence")
        .and_then(|v| v.as_str())
        .and_then(SafetyPersistence::parse);
    (
        scope.unwrap_or_default(),
        authority.unwrap_or_default(),
        persistence.unwrap_or_default(),
    )
}

/// 小写字母数字 token 集 (len>=2)。会话冗余 Jaccard 用。
pub fn token_set(text: &str) -> std::collections::HashSet<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .map(|t| t.to_lowercase())
        .filter(|t| t.chars().count() >= 2)
        .collect()
}

/// Jaccard 相似度 |A∩B| / |A∪B|。空集对空集 = 0.0 (无信息不算冗余)。
pub fn jaccard(
    a: &std::collections::HashSet<String>,
    b: &std::collections::HashSet<String>,
) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let inter = a.intersection(b).count();
    let union = a.union(b).count();
    if union == 0 {
        return 0.0;
    }
    inter as f64 / union as f64
}

/// 权威层级的新颖性阈值倍率: observed 需 2x 证据, team 1.5x, self/system 基准。
pub fn authority_novelty_multiplier(authority: SafetyAuthority) -> f32 {
    match authority {
        SafetyAuthority::SelfAuth => 1.0,
        SafetyAuthority::Observed => 2.0,
        SafetyAuthority::Team => 1.5,
        SafetyAuthority::System => 0.0,
    }
}

/// 准入裁决。cand_sim_max = 候选向量对全库 KNN top-1 cosine (候选尚未插入, 天然排除自身);
/// session_traces = 同 session 近期活跃记忆内容 (SESSION_TRACE_WINDOW 条)。
pub fn evaluate(
    candidate: &str,
    cand_sim_max: f32,
    session_traces: &[String],
    cfg: &AdmissionConfig,
    authority: SafetyAuthority,
) -> AdmissionVerdict {
    let novelty = (1.0 - cand_sim_max.clamp(0.0, 1.0)).max(0.0);
    // system 权威: 系统写入 (运维/巩固产物通道) 恒准入。
    if authority == SafetyAuthority::System {
        return AdmissionVerdict {
            accepted: true,
            score: 1.0,
            reason: "system-authority".into(),
        };
    }
    // 规则 1: 近重复复述 — 重放不改变经验分布。
    if cand_sim_max >= cfg.dup_sim {
        return AdmissionVerdict {
            accepted: false,
            score: novelty,
            reason: "duplicate".into(),
        };
    }
    // 规则 2: 会话冗余 — 轨迹里已有同等信息, 存了也是 no-op。
    let cand_tokens = token_set(candidate);
    for trace in session_traces {
        if jaccard(&cand_tokens, &token_set(trace)) >= SESSION_REDUNDANT_JACCARD {
            return AdmissionVerdict {
                accepted: false,
                score: novelty,
                reason: "session-redundant".into(),
            };
        }
    }
    // 规则 3: 低权威需更高新颖性。
    let threshold = cfg.min_novelty * authority_novelty_multiplier(authority);
    if novelty < threshold {
        return AdmissionVerdict {
            accepted: false,
            score: novelty,
            reason: "low-novelty".into(),
        };
    }
    AdmissionVerdict {
        accepted: true,
        score: novelty,
        reason: "accepted".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> AdmissionConfig {
        AdmissionConfig::default()
    }

    #[test]
    fn duplicate_rejected() {
        let v = evaluate("same text", 0.97, &[], &cfg(), SafetyAuthority::SelfAuth);
        assert!(!v.accepted);
        assert_eq!(v.reason, "duplicate");
    }

    #[test]
    fn session_redundant_rejected() {
        let traces = vec!["deploy the service on friday afternoon".to_string()];
        let v = evaluate(
            "deploy the service on friday afternoon",
            0.2,
            &traces,
            &cfg(),
            SafetyAuthority::SelfAuth,
        );
        assert!(!v.accepted);
        assert_eq!(v.reason, "session-redundant");
        // 同 token 集的变体措辞同样命中 (Jaccard 对语序不敏感)。
        let v2 = evaluate(
            "on friday afternoon deploy the service",
            0.2,
            &traces,
            &cfg(),
            SafetyAuthority::SelfAuth,
        );
        assert!(!v2.accepted);
    }

    #[test]
    fn novel_accepted_and_scored() {
        let v = evaluate(
            "totally new insight about retry policy",
            0.05,
            &["unrelated trace content".to_string()],
            &cfg(),
            SafetyAuthority::SelfAuth,
        );
        assert!(v.accepted);
        assert!(v.score > 0.9);
        assert_eq!(v.reason, "accepted");
    }

    #[test]
    fn low_novelty_rejected_for_self_but_system_accepted() {
        let v = evaluate("x", 0.93, &[], &cfg(), SafetyAuthority::SelfAuth);
        assert!(!v.accepted);
        assert_eq!(v.reason, "low-novelty");
        let sys = evaluate("x", 0.99, &[], &cfg(), SafetyAuthority::System);
        assert!(sys.accepted);
        assert_eq!(sys.reason, "system-authority");
    }

    #[test]
    fn observed_authority_needs_double_novelty() {
        // novelty 0.15 > self 阈值 0.10 (通过) 但 < observed 阈值 0.20 (拒绝)。
        let self_v = evaluate("q", 0.85, &[], &cfg(), SafetyAuthority::SelfAuth);
        let obs_v = evaluate("q", 0.85, &[], &cfg(), SafetyAuthority::Observed);
        assert!(self_v.accepted);
        assert!(!obs_v.accepted);
        assert_eq!(obs_v.reason, "low-novelty");
    }

    #[test]
    fn safety_metadata_extraction() {
        let md =
            serde_json::json!({"scope": "agent", "authority": "observed", "persistence": "pinned"});
        let (s, a, p) = safety_from_metadata(&md);
        assert_eq!(s, SafetyScope::Agent);
        assert_eq!(a, SafetyAuthority::Observed);
        assert_eq!(p, SafetyPersistence::Pinned);
        let (s, a, p) = safety_from_metadata(&serde_json::json!({}));
        assert_eq!(s, SafetyScope::Session);
        assert_eq!(a, SafetyAuthority::SelfAuth);
        assert_eq!(p, SafetyPersistence::Normal);
        // 非法值降级默认, 不 panic。
        let md = serde_json::json!({"scope": "bogus", "authority": 42});
        let (s, a, _) = safety_from_metadata(&md);
        assert_eq!(s, SafetyScope::Session);
        assert_eq!(a, SafetyAuthority::SelfAuth);
    }

    #[test]
    fn jaccard_edges() {
        let a = token_set("alpha beta");
        let b = token_set("alpha beta");
        assert!((jaccard(&a, &b) - 1.0).abs() < 1e-9);
        let c = token_set("gamma delta");
        assert!(jaccard(&a, &c).abs() < 1e-9);
        assert!(jaccard(&a, &std::collections::HashSet::new()).abs() < 1e-9);
    }
}
