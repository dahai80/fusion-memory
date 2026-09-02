//! 一次性 M2 活体验收 example (真实模型)。验收后删除, 仅留日志。PRD line 923。
//! 跑法: FUSION_MEMORY_MLX_API_KEY=change-me cargo run -p fm-cli --example live_acceptance

use std::sync::Arc;

use fm_core::{EntityNode, EntityType, MemoryItem, MemoryType};
use fm_embed::{EmbedConfig, Embedder, MlxEmbedder};
use fm_engine::entity_extract::{
    parse_extraction, EntityExtractor, ExtractConfig, MlxEntityExtractor,
};
use fm_graph::align_entity;
use fm_persist::Persist;

fn main() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let cfg = ExtractConfig {
        mlx_url: "http://127.0.0.1:11434/v1".into(),
        api_key: "change-me".into(),
        chat_model: "Qwen3.5-9B-4bit".into(),
        timeout_secs: 60,
    };
    let ext = MlxEntityExtractor::new(cfg).unwrap();
    let samples = [
        "用户在用 Rust 写 fusion-memory 项目, 偏好4空格缩进。Alice 做了 SQLite 持久化。",
        "I prefer Python for data science. The project is deployed on Kubernetes.",
        "Always use 4 space indentation. Never commit secrets to git.",
        "See https://hf-mirror.com to download bge-m3 model.",
        "今天跑了 fusion-cli 的 cargo build, 修了一个 clippy warning。",
    ];
    let mut ok = 0usize;
    for s in &samples {
        let r = rt.block_on(ext.extract(s));
        println!("[extract] ok={} n={}", r.success, r.entities.len());
        if r.success {
            ok += 1;
        }
    }
    let rate = 100.0 * ok as f64 / samples.len() as f64;
    println!(
        "PARSE_SUCCESS_RATE = {}/{} = {:.0}%  (要求 >90%)",
        ok,
        samples.len(),
        rate
    );
    assert!(rate > 90.0, "实体抽取 JSON 解析成功率不达标");

    // 纯函数 parse_extraction 对照 (markdown fence 容错)
    let fenced = "```json\n[{\"name\":\"Rust\",\"entity_type\":\"Tech\"}]\n```";
    let pr = parse_extraction(fenced);
    assert!(
        pr.success && pr.entities.len() == 1,
        "parse_extraction markdown fence 失败"
    );
    println!("[parse_extraction] markdown-fence ok");

    // rule-priority 对齐: 规则1 同名同 type 合并; 同名异 type 不合并; 规则3 alias 合并
    let p = Persist::open_in_memory().unwrap();
    let seed = MemoryItem::new_turn_skeleton(
        "m0".into(),
        "ix".into(),
        0,
        "s".into(),
        String::new(),
        MemoryType::Episodic,
        "c".into(),
        1,
    );
    let mut seed = seed;
    seed.entities = vec![EntityNode {
        id: "ent-rust".into(),
        name: "Rust".into(),
        aliases: vec!["rust-lang".into()],
        entity_type: EntityType::Tech,
    }];
    p.put_memory(&seed).unwrap();

    let cand1 = EntityNode {
        id: "ent-rust-2".into(),
        name: "Rust".into(),
        aliases: vec![],
        entity_type: EntityType::Tech,
    };
    let o1 = align_entity(&p, &cand1, None).unwrap();
    println!(
        "[rule1] same-name-same-type: id={} pri={} merged={}",
        o1.canonical_id, o1.rule_priority, o1.merged
    );
    assert_eq!(o1.canonical_id, "ent-rust");
    assert_eq!(o1.rule_priority, 3);

    let cand2 = EntityNode {
        id: "ent-rust-c".into(),
        name: "Rust".into(),
        aliases: vec![],
        entity_type: EntityType::Concept,
    };
    let o2 = align_entity(&p, &cand2, None).unwrap();
    println!("[no-merge] same-name-diff-type: merged={}", o2.merged);
    assert!(!o2.merged, "同名异 type 不可合并");

    let cand3 = EntityNode {
        id: "ent-rustlang".into(),
        name: "rust-lang".into(),
        aliases: vec![],
        entity_type: EntityType::Tech,
    };
    let o3 = align_entity(&p, &cand3, None).unwrap();
    println!(
        "[rule3] alias-match: id={} pri={}",
        o3.canonical_id, o3.rule_priority
    );
    assert_eq!(o3.canonical_id, "ent-rust");

    println!("RULE_PRIORITY_OK");
    println!("M2_LIVE_ACCEPTANCE_PASS");

    // 真 bge-m3 embedding 往返 (MlxEmbedder 路径, 非 curl)
    let ecfg = EmbedConfig {
        api_key: "change-me".into(),
        ..EmbedConfig::from_env()
    };
    let emb = MlxEmbedder::new(ecfg).unwrap();
    let v = rt
        .block_on(emb.embed("fusion-memory 真实嵌入往返验收"))
        .unwrap();
    assert_eq!(v.len(), 1024, "bge-m3 dim=1024");
    let v2 = rt
        .block_on(emb.embed("fusion-memory 真实嵌入往返验收"))
        .unwrap();
    // 同文本缓存命中 → 同向量
    assert_eq!(v, v2, "同文本 embedding 应一致 (缓存)");
    let v3 = rt
        .block_on(emb.embed("completely different text here"))
        .unwrap();
    assert_eq!(v3.len(), 1024);
    assert_ne!(v, v3, "异文本 embedding 应不同");
    println!("[embed] round-trip dim=1024 cache-hit ok, MlxEmbedder path verified");

    let _ = Arc::new(());
}
