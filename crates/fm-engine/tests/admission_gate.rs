//! #57/#58: 经验准入门 + agent 命名空间隔离 e2e (离线, StubEmbedder)。
//!
//! StubEmbedder 滑窗 hash: 相同/近同文本向量高度相关 → 近重复拒绝路径可离线驱动。
//! 验收项对应 issue:
//! - 准入门在有记录 episode 集上拒绝 no-op 候选 (deterministic replay traces)
//! - 每条记忆带 scope/authority/persistence; 撤销端点按元数据 (不止按 id) 删
//! - gated store ≥ ungated quality 且条目更少
//! - 两 agent 写同内容互不可见, 显式授权后可见; publish → 共享仓库可见; adopt 过门

use std::sync::Arc;

use fm_core::{
    CommitOutcome, FusionMemoryEngine, Interaction, MemoryItem, MemoryType, RetrieveQuery,
    SafetyAuthority, SafetyPersistence, SafetyScope, ShareGrant, Turn,
};
use fm_embed::StubEmbedder;
use fm_engine::{admission, MemoryEngine};
use fm_persist::Persist;
use fm_store::LocalStore;

const DIM: usize = 64;

fn engine(gate: bool, dir: &str) -> MemoryEngine {
    let store = Arc::new(LocalStore::open(dir, DIM).unwrap());
    let persist = Arc::new(Persist::open_in_memory().unwrap());
    let embedder: Arc<dyn EmbedderT> = Arc::new(StubEmbedder::new(DIM));
    let eng = MemoryEngine::new(store, persist, embedder);
    if gate {
        eng.with_admission(admission::AdmissionConfig {
            enabled: true,
            ..Default::default()
        })
    } else {
        eng
    }
}

use fm_embed::Embedder as EmbedderT;

fn ix(
    id: &str,
    session: &str,
    ns: &str,
    key: &str,
    contents: &[&str],
    meta: serde_json::Value,
) -> Interaction {
    Interaction {
        id: id.into(),
        session_id: session.into(),
        tenant: String::new(),
        namespace: ns.into(),
        namespace_key: key.into(),
        turns: contents
            .iter()
            .enumerate()
            .map(|(i, c)| Turn {
                turn_idx: i as u32,
                user_message: c.to_string(),
                assistant_message: String::new(),
                tool_calls: vec![],
            })
            .collect(),
        timestamp: 0,
        owner: None,
        metadata: meta,
    }
}

fn tmp_dir(tag: &str) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static C: AtomicU64 = AtomicU64::new(0);
    let n = C.fetch_add(1, Ordering::SeqCst);
    let d = std::env::temp_dir().join(format!("fm-admission-{tag}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d.to_string_lossy().into_owned()
}

/// 验收 1: 门关 — 重复 episode 全量入库 (旧行为)。
#[tokio::test]
async fn gate_off_stores_all() {
    let d = tmp_dir("off");
    let eng = engine(false, &d);
    let a = ix(
        "ix-a",
        "s1",
        "",
        "",
        &["lesson: retry twice then escalate"],
        serde_json::json!({}),
    );
    let out_a: CommitOutcome = eng.commit_episodic_memory_detailed("s1", &a).await.unwrap();
    assert_eq!(out_a.memory_ids.len(), 1);
    let b = ix(
        "ix-b",
        "s2",
        "",
        "",
        &["lesson: retry twice then escalate"],
        serde_json::json!({}),
    );
    let out_b = eng.commit_episodic_memory_detailed("s2", &b).await.unwrap();
    assert_eq!(
        out_b.memory_ids.len(),
        1,
        "gate off: duplicate replay stored"
    );
    assert!(out_b.rejected_turns.is_empty());
}

/// 验收 1: 门开 — no-op 候选 (重复 replay) 被拒, 裁决随首条记忆落库。
#[tokio::test]
async fn gate_rejects_duplicate_replay() {
    let d = tmp_dir("dup");
    let eng = engine(true, &d);
    let a = ix(
        "ix-a",
        "s1",
        "",
        "",
        &["lesson: retry twice then escalate"],
        serde_json::json!({}),
    );
    let out_a = eng.commit_episodic_memory_detailed("s1", &a).await.unwrap();
    assert_eq!(out_a.memory_ids.len(), 1);
    // 首条: 门接受, 裁决记录在案。
    let first = eng
        .persist()
        .get_memory(&out_a.memory_ids[0].0)
        .unwrap()
        .unwrap();
    let rec = first.admission.as_ref().expect("admission recorded");
    assert!(rec.accepted);
    assert_eq!(rec.gate_version, admission::GATE_VERSION);
    // safety metadata 默认值在每条记忆上。
    assert_eq!(first.scope, SafetyScope::Session);
    assert_eq!(first.authority, SafetyAuthority::SelfAuth);
    assert_eq!(first.persistence, SafetyPersistence::Normal);

    let b = ix(
        "ix-b",
        "s2",
        "",
        "",
        &["lesson: retry twice then escalate"],
        serde_json::json!({}),
    );
    let out_b = eng.commit_episodic_memory_detailed("s2", &b).await.unwrap();
    assert!(
        out_b.memory_ids.is_empty(),
        "gate on: duplicate replay must be rejected"
    );
    assert_eq!(out_b.rejected_turns.len(), 1);
    assert_eq!(out_b.rejected_turns[0].stage, "admission");
    assert_eq!(out_b.rejected_turns[0].error, "duplicate");
    // 新内容 (真正新颖) 仍通过 — 门不搞一刀切。
    let c = ix(
        "ix-c",
        "s3",
        "",
        "",
        &["lesson: check disk quota before deploying"],
        serde_json::json!({}),
    );
    let out_c = eng.commit_episodic_memory_detailed("s3", &c).await.unwrap();
    assert_eq!(out_c.memory_ids.len(), 1, "novel lesson must pass");
}

/// 验收 2: interaction.metadata 传 safety metadata (valid_from 同款自由字段), pinned 不回收。
#[tokio::test]
async fn safety_metadata_and_pinned() {
    let d = tmp_dir("safety");
    let eng = engine(false, &d);
    let a = ix(
        "ix-a",
        "s1",
        "",
        "",
        &["observed behaviour: worker stalls on locked table"],
        serde_json::json!({"scope": "agent", "authority": "observed", "persistence": "pinned"}),
    );
    let out = eng.commit_episodic_memory_detailed("s1", &a).await.unwrap();
    let item: MemoryItem = eng
        .persist()
        .get_memory(&out.memory_ids[0].0)
        .unwrap()
        .unwrap();
    assert_eq!(item.scope, SafetyScope::Agent);
    assert_eq!(item.authority, SafetyAuthority::Observed);
    assert_eq!(item.persistence, SafetyPersistence::Pinned);
    // pinned: 远期时间点不触发回收 (THETA_DROP)。纯函数对照用合成条目 (秒级时间轴,
    // 与 retrieval_quality 消融同款; 引擎 commit 走 ms 时钟, 衰减常数语义不变)。
    let far = item.created_timestamp + 400 * 86_400;
    assert!(!fm_engine::scoring::should_recycle(&item, far));
    let mid = 1_800_000_000 + 2 * 86_400;

    fn synth(authority: SafetyAuthority, persistence: SafetyPersistence) -> MemoryItem {
        let mut m = MemoryItem::new_turn_skeleton(
            "t".into(),
            "ix".into(),
            0,
            "s".into(),
            String::new(),
            MemoryType::Episodic,
            "content".into(),
            1_800_000_000,
        );
        m.authority = authority;
        m.persistence = persistence;
        m
    }
    let pinned = synth(SafetyAuthority::SelfAuth, SafetyPersistence::Pinned);
    let normal_self = synth(SafetyAuthority::SelfAuth, SafetyPersistence::Normal);
    let normal_observed = synth(SafetyAuthority::Observed, SafetyPersistence::Normal);
    assert!(!fm_engine::scoring::should_recycle(&pinned, far));
    // observed 衰减快于 self (有效年龄 2x); pinned 恒 w0 不衰减。
    let w_pinned = fm_engine::scoring::weight_of(&pinned, mid);
    let w_self = fm_engine::scoring::weight_of(&normal_self, mid);
    let w_observed = fm_engine::scoring::weight_of(&normal_observed, mid);
    assert!(
        w_pinned > w_self,
        "pinned never decays: {w_pinned} > {w_self}"
    );
    assert!(
        w_observed < w_self,
        "low authority decays faster: {w_observed} < {w_self}"
    );
    // observed 有效年龄 4 天 → 已过 θ_drop 被回收, self 2 天仍存活。
    assert!(!fm_engine::scoring::should_recycle(&normal_self, mid));
    assert!(fm_engine::scoring::should_recycle(&normal_observed, mid));
    // freshness 同款单调性 (检索排序层)。
    assert!(
        fm_engine::scoring::freshness_of(&pinned, far)
            >= fm_engine::scoring::freshness_of(&normal_self, far)
    );
}

/// 验收 2: 撤销端点按元数据 (不止按 id) — authority=observed 全撤, self 留; 空过滤拒绝。
#[tokio::test]
async fn revoke_by_metadata() {
    let d = tmp_dir("revoke");
    let eng = engine(false, &d);
    let obs = ix(
        "ix-obs",
        "s1",
        "",
        "",
        &["observed: table lock stalls worker"],
        serde_json::json!({"authority": "observed"}),
    );
    let own = ix(
        "ix-own",
        "s2",
        "",
        "",
        &["self lesson: warm the cache before benchmarking"],
        serde_json::json!({}),
    );
    let out_obs = eng
        .commit_episodic_memory_detailed("s1", &obs)
        .await
        .unwrap();
    let out_own = eng
        .commit_episodic_memory_detailed("s2", &own)
        .await
        .unwrap();
    let filter = fm_core::RevokeFilter {
        authority: Some("observed".into()),
        ..Default::default()
    };
    let n = eng.revoke_by_meta(&filter, "").await.unwrap();
    assert_eq!(n, 1, "only the observed-authority item revoked");
    let gone = eng
        .persist()
        .get_memory(&out_obs.memory_ids[0].0)
        .unwrap()
        .unwrap();
    assert!(gone.tombstone);
    let kept = eng
        .persist()
        .get_memory(&out_own.memory_ids[0].0)
        .unwrap()
        .unwrap();
    assert!(!kept.tombstone, "self-authority item must survive");
    // 空过滤器 fail-closed。
    let err = eng
        .revoke_by_meta(&fm_core::RevokeFilter::default(), "")
        .await;
    assert!(err.is_err(), "empty filter must be refused");
}

/// 验收 3 (benchmark): gated store ≥ ungated quality 且条目更少 (确定性 episode 集)。
#[tokio::test]
async fn bench_gated_not_below_ungated_with_fewer_items() {
    let facts = [
        "runbook: restart the ingest worker before pivoting the topic",
        "lesson: quota errors surface as 403 in the staging cluster",
        "insight: warm the embedding cache before throughput runs",
        "fact: the release train leaves tuesday after the signoff",
        "pattern: pin the model version before comparing benchmarks",
        "note: drain the queue before shutting the collector down",
    ];
    // no-op candidates: 对已存事实的逐字复述 (Anchor 语义的重复 replay)。
    let noops = [
        "runbook: restart the ingest worker before pivoting the topic",
        "insight: warm the embedding cache before throughput runs",
        "pattern: pin the model version before comparing benchmarks",
        "note: drain the queue before shutting the collector down",
    ];

    async fn run(
        gate: bool,
        facts: &[&str],
        noops: &[&str],
        dir: &str,
    ) -> (usize, Vec<Vec<String>>) {
        let eng = engine(gate, dir);
        for (i, f) in facts.iter().enumerate() {
            let a = ix(
                &format!("ix-f{i}"),
                &format!("s{i}"),
                "",
                "",
                &[f],
                serde_json::json!({}),
            );
            eng.commit_episodic_memory_detailed(&format!("s{i}"), &a)
                .await
                .unwrap();
        }
        for (i, n) in noops.iter().enumerate() {
            let a = ix(
                &format!("ix-n{i}"),
                &format!("sn{i}"),
                "",
                "",
                &[n],
                serde_json::json!({}),
            );
            eng.commit_episodic_memory_detailed(&format!("sn{i}"), &a)
                .await
                .unwrap();
        }
        let count = eng.count(None).unwrap();
        // 质量: 每条事实文本作 query, recall@1 (top block 含同 interaction)。
        let mut recalls = Vec::new();
        for (i, f) in facts.iter().enumerate() {
            let q = RetrieveQuery {
                text: f.to_string(),
                top_k: 3,
                tenant: String::new(),
                namespace: String::new(),
                namespace_key: String::new(),
                session_id: None,
                tier_filter: None,
                token_budget: 4096,
                aggregate: true,
            };
            let ctx = eng.retrieve_context(&q).await.unwrap();
            let hit = ctx
                .blocks
                .iter()
                .any(|b| b.interaction_id == format!("ix-f{i}"));
            recalls.push(vec![hit.to_string()]);
        }
        (count as usize, recalls)
    }

    let d1 = tmp_dir("bench-ungated");
    let (ungated_count, ungated_recall) = run(false, &facts, &noops, &d1).await;
    let d2 = tmp_dir("bench-gated");
    let (gated_count, gated_recall) = run(true, &facts, &noops, &d2).await;

    assert_eq!(ungated_count, facts.len() + noops.len());
    assert_eq!(
        gated_count,
        facts.len(),
        "gated store keeps only novel items"
    );
    // 质量 floor: 逐条事实 recall 不低于 ungated。
    for (i, (u, g)) in ungated_recall.iter().zip(gated_recall.iter()).enumerate() {
        let u_hit = u[0] == "true";
        let g_hit = g[0] == "true";
        assert!(
            g_hit >= u_hit,
            "fact {i}: gated recall must not drop (ungated={u_hit} gated={g_hit})"
        );
    }
}

/// 验收 4 (#58): 两 agent 写同内容互不可见; 显式授权可见; publish/adopt 过门。
#[tokio::test]
async fn agent_isolation_grants_publish_adopt() {
    let d = tmp_dir("agents");
    let eng = engine(false, &d);
    let content = "identical lesson: flush the connector before schema drift checks";
    let a = ix(
        "ix-a",
        "sa",
        "agent",
        "agentA",
        &[content],
        serde_json::json!({}),
    );
    let b = ix(
        "ix-b",
        "sb",
        "agent",
        "agentB",
        &[content],
        serde_json::json!({}),
    );
    eng.commit_episodic_memory_detailed("sa", &a).await.unwrap();
    eng.commit_episodic_memory_detailed("sb", &b).await.unwrap();

    let q_a = RetrieveQuery {
        text: content.to_string(),
        top_k: 10,
        tenant: String::new(),
        namespace: "agent".into(),
        namespace_key: "agentA".into(),
        session_id: None,
        tier_filter: None,
        token_budget: 8192,
        aggregate: true,
    };
    let ctx_a = eng.retrieve_context(&q_a).await.unwrap();
    assert!(
        ctx_a.blocks.iter().all(|blk| blk.interaction_id != "ix-b"),
        "agentA must not see agentB records without grant"
    );
    assert!(ctx_a.blocks.iter().any(|blk| blk.interaction_id == "ix-a"));

    // 显式授权: agentA 授 agentB → B 检索可见 A 的记录。
    eng.grant_access(&ShareGrant {
        grantor_key: "agentA".into(),
        grantee_key: "agentB".into(),
        namespace: "agent".into(),
        created_at: 0,
    })
    .await
    .unwrap();
    let grants = eng.list_grants("agentB", "agent").await.unwrap();
    assert_eq!(grants.len(), 1);
    let q_b = RetrieveQuery {
        text: content.to_string(),
        top_k: 10,
        tenant: String::new(),
        namespace: "agent".into(),
        namespace_key: "agentB".into(),
        session_id: None,
        tier_filter: None,
        token_budget: 8192,
        aggregate: true,
    };
    let ctx_b = eng.retrieve_context(&q_b).await.unwrap();
    let ids: Vec<&str> = ctx_b
        .blocks
        .iter()
        .map(|blk| blk.interaction_id.as_str())
        .collect();
    assert!(
        ids.contains(&"ix-a") && ids.contains(&"ix-b"),
        "grantee sees grantor records: {ids:?}"
    );
    // 撤销授权 → 回到隔离。
    assert_eq!(
        eng.revoke_access("agentA", "agentB", "agent")
            .await
            .unwrap(),
        1
    );
    let ctx_b2 = eng.retrieve_context(&q_b).await.unwrap();
    assert!(
        ctx_b2.blocks.iter().all(|blk| blk.interaction_id != "ix-a"),
        "revoked grant must re-isolate"
    );

    // publish: A 的独立洞见进共享仓库 (playbook, team 权威, pinned)。
    let insight = ix(
        "ix-ins",
        "sa",
        "agent",
        "agentA",
        &["team insight: prefer idempotent retries for webhook fanout"],
        serde_json::json!({}),
    );
    let out_ins = eng
        .commit_episodic_memory_detailed("sa", &insight)
        .await
        .unwrap();
    let pb_id = eng
        .publish_playbook(&out_ins.memory_ids[0].0, "agentA", "")
        .await
        .unwrap();
    let pb = eng.persist().get_memory(&pb_id).unwrap().unwrap();
    assert_eq!(pb.namespace, "playbook");
    assert_eq!(pb.namespace_key, "agentA");
    assert_eq!(pb.authority, SafetyAuthority::Team);
    assert_eq!(pb.persistence, SafetyPersistence::Pinned);
    assert_eq!(pb.sources, vec![out_ins.memory_ids[0].0.clone()]);
    // 共享仓库租户内全员可见。
    let q_pb = RetrieveQuery {
        text: "idempotent retries webhook fanout".into(),
        top_k: 5,
        tenant: String::new(),
        namespace: "playbook".into(),
        namespace_key: String::new(),
        session_id: None,
        tier_filter: None,
        token_budget: 8192,
        aggregate: true,
    };
    let ctx_pb = eng.retrieve_context(&q_pb).await.unwrap();
    assert!(
        ctx_pb
            .blocks
            .iter()
            .any(|blk| blk.interaction_id == pb.interaction_id),
        "published insight visible in shared repo"
    );

    // adopt: B 采纳 → 过准入门, 入 B 命名空间, 裁决记录 accepted + sources 溯源。
    let adopt_id = eng
        .adopt_playbook(&pb_id, "agent", "agentB", "")
        .await
        .unwrap()
        .expect("novel playbook must be adoptable");
    let adopted = eng.persist().get_memory(&adopt_id).unwrap().unwrap();
    assert_eq!(adopted.namespace, "agent");
    assert_eq!(adopted.namespace_key, "agentB");
    assert_eq!(adopted.authority, SafetyAuthority::Team);
    let rec = adopted.admission.as_ref().expect("adopt records admission");
    assert!(rec.accepted);
    assert_eq!(adopted.sources, vec![pb_id.clone()]);
    // 盲传播拦截: 重复采纳 (B 已有同内容) → 门拒绝 (Ok(None), 不落库)。
    let dup_adopt = eng
        .adopt_playbook(&pb_id, "agent", "agentB", "")
        .await
        .unwrap();
    assert!(
        dup_adopt.is_none(),
        "re-adopt of identical content must be gate-rejected"
    );
}

/// 巩固产物通道: summarize 语义 (这里以 publish 为代表) — 门内 system 权威恒准入。
#[tokio::test]
async fn system_authority_always_admitted() {
    let d = tmp_dir("system");
    let eng = engine(true, &d);
    let content = "system note: schema v10 migration adds share_grant";
    let a = ix("ix-a", "s1", "", "", &[content], serde_json::json!({}));
    eng.commit_episodic_memory_detailed("s1", &a).await.unwrap();
    let b = ix(
        "ix-b",
        "s2",
        "",
        "",
        &[content],
        serde_json::json!({"authority": "system"}),
    );
    let out = eng.commit_episodic_memory_detailed("s2", &b).await.unwrap();
    assert_eq!(
        out.memory_ids.len(),
        1,
        "system authority bypasses dup rejection (ops channel)"
    );
    let _ = MemoryType::Episodic; // silence unused import in cfg variations
}
