//! M4 #41: LongMemEval-subset CI gate — retrieval pipeline quality on
//! LongMemEval-shaped data (needle facts in session haystacks) through the REAL
//! engine path (commit → retrieve_context, four-way fusion).
//!
//! Two embedder modes (plan §2.1.3 dual-mode):
//!   - stub (default, CI): StubEmbedder — deterministic, offline, pipeline floor.
//!   - live (feature mlx-live, --include-ignored): real bge-m3 via fusion-mlx.
//!
//! Baseline JSON committed at docs/benchmarks/longmemeval-subset-baseline.json
//! (defense line 6: reported numbers always ship with a repro command):
//!   cargo test -p fm-engine --test longmemeval_subset -- --nocapture
//!   FM_LME_UPDATE=1 cargo test ...   # regenerate baseline (verify no regression first)
//!   cargo test -p fm-engine --features mlx-live --test longmemeval_subset -- --include-ignored

use std::path::PathBuf;

use fm_core::{FusionMemoryEngine, Interaction, RetrieveQuery, Turn};
use fm_engine::MemoryEngine;
use fm_persist::Persist;
use fm_store::LocalStore;

const SEED: u64 = 0x10ae_4e51_1111_0001;
const DIM: usize = 64;

/// 确定性 xorshift64* RNG (与 retrieval_quality.rs 同款, 零依赖)。
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed ^ 0x9E37_79B9_7F4A_7C15)
    }
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next_u64() % n
    }
}

/// needle 主题清单 (代号/设备/凭证类 — 专有名词, 词法路主战场)。
const NEEDLES: &[&str] = &[
    "lab-router",
    "vacuum-dock",
    "ci-runner",
    "backup-nas",
    "door-lock",
    "printer-foomatic",
    "solar-inverter",
    "k8s-ingress",
    "vpn-gateway",
    "brew-kegerator",
    "thermostat-east",
    "camera-garage",
    "audio-amp",
    "robot-lawnmower",
    "water-meter",
    "smoke-detector",
    "ev-charger",
    "greenhouse-pump",
    "cat-feeder",
    "ham-radio",
];

struct Instance {
    evidence_ix: String,
    question: String,
}

/// 构造一个 LongMemEval 形状语料: 每 instance 一个证据 session (needle fact)
/// 加一个近失 session (同关键词但无答案, 词法对抗) 加 filler haystack sessions。
/// 全部经 engine.commit_episodic_memory 真实落库 (含 FTS 同步 + 向量)。
async fn build_corpus(engine: &MemoryEngine, rng: &mut Rng) -> Vec<Instance> {
    let mut instances = Vec::new();
    let mut ix_seq = 0u64;
    let mut next_ix = |tag: &str| {
        ix_seq += 1;
        format!("lme-{tag}-{ix_seq}")
    };

    fn interaction(id: &str, topic: &str, turns: Vec<String>) -> Interaction {
        Interaction {
            id: id.into(),
            session_id: format!("sess-{topic}"),
            tenant: String::new(),
            namespace: String::new(),
            namespace_key: String::new(),
            turns: turns
                .into_iter()
                .enumerate()
                .map(|(i, user_message)| Turn {
                    turn_idx: i as u32,
                    user_message,
                    assistant_message: String::new(),
                    tool_calls: vec![],
                })
                .collect(),
            timestamp: 0,
            metadata: Default::default(),
        }
    }

    for (t, topic) in NEEDLES.iter().enumerate() {
        let code = format!("AC-{t:04}-XK");
        let filler_a = format!("standup notes sprint {}", rng.below(50) + 1);
        let filler_b = format!("lunch order round table {}", rng.below(90) + 1);

        // 证据 session: needle fact + 噪声 turns
        let evid_ix = next_ix("evid");
        engine
            .commit_episodic_memory(
                &format!("sess-{topic}"),
                &interaction(
                    &evid_ix,
                    topic,
                    vec![
                        filler_a.clone(),
                        format!("The maintenance access code for {topic} is {code}."),
                        filler_b.clone(),
                    ],
                ),
            )
            .await
            .unwrap();

        // 近失 session: 同关键词, 无答案 (词法对抗 — BM25 必然召回, 排序须压过)
        let miss_ix = next_ix("miss");
        engine
            .commit_episodic_memory(
                &format!("sess-{topic}-miss"),
                &interaction(
                    &miss_ix,
                    topic,
                    vec![format!(
                        "The maintenance access code for {topic} was rotated out last quarter."
                    )],
                ),
            )
            .await
            .unwrap();

        // filler haystack: 无关 session
        for f in 0..6 {
            let f_ix = next_ix("fill");
            engine
                .commit_episodic_memory(
                    &format!("sess-{topic}-fill{f}"),
                    &interaction(
                        &f_ix,
                        topic,
                        vec![format!(
                            "unrelated note {}: chassis review or meeting room booking",
                            rng.below(1000)
                        )],
                    ),
                )
                .await
                .unwrap();
        }

        instances.push(Instance {
            evidence_ix: evid_ix,
            question: format!("What is the maintenance access code for {topic}?"),
        });
    }
    instances
}

fn baseline_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/benchmarks/longmemeval-subset-baseline.json")
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
struct Baseline {
    mode: String,
    instances: usize,
    recall_at_5: f64,
    mrr: f64,
}

/// 跑评测: 逐 question 检索, 证据 interaction 命中 top-5 blocks 记 recall, 1/MRR。
async fn evaluate(engine: &MemoryEngine, instances: &[Instance]) -> (f64, f64) {
    let mut recall_sum = 0.0f64;
    let mut mrr_sum = 0.0f64;
    let n = instances.len() as f64;
    let verbose = std::env::var("FM_LME_VERBOSE").as_deref() == Ok("1");
    for inst in instances {
        let q = RetrieveQuery::new(&inst.question, 5, 4000);
        let ctx = engine.retrieve_context(&q).await.unwrap();
        let pos = ctx
            .blocks
            .iter()
            .position(|b| b.interaction_id == inst.evidence_ix);
        if verbose {
            println!(
                "Q: {} | evidence@{:?} | blocks: {}",
                inst.question,
                pos,
                ctx.blocks
                    .iter()
                    .take(6)
                    .map(|b| format!("{}({:.3})", b.interaction_id, b.score))
                    .collect::<Vec<_>>()
                    .join(" ")
            );
        }
        if let Some(p) = pos {
            if p < 5 {
                recall_sum += 1.0;
            }
            mrr_sum += 1.0 / (p + 1) as f64;
        }
    }
    (recall_sum / n, mrr_sum / n)
}

fn assert_meets_baseline(actual: Baseline) {
    let path = baseline_path();
    if std::env::var("FM_LME_UPDATE").as_deref() == Ok("1") {
        std::fs::write(&path, serde_json::to_string_pretty(&actual).unwrap() + "\n")
            .expect("write baseline");
        println!("baseline updated: {}", path.display());
        return;
    }
    let raw = std::fs::read_to_string(&path).expect("baseline JSON must exist");
    let base: Baseline = serde_json::from_str(&raw).expect("baseline JSON parse");
    assert_eq!(base.mode, actual.mode, "mode mismatch");
    assert_eq!(base.instances, actual.instances, "instance count changed");
    assert!(
        actual.recall_at_5 >= base.recall_at_5 - 1e-9,
        "recall@5 {} < baseline {} — retrieval regression",
        actual.recall_at_5,
        base.recall_at_5
    );
    assert!(
        actual.mrr >= base.mrr - 1e-9,
        "MRR {} < baseline {} — retrieval regression",
        actual.mrr,
        base.mrr
    );
}

/// CI gate (stub mode): pipeline floor — evidence recall@5 与 MRR 不得低于 baseline。
#[tokio::test]
async fn longmemeval_stub_never_below_baseline() {
    let mut rng = Rng::new(SEED);
    let dir = tempfile::tempdir().unwrap();
    let store = std::sync::Arc::new(LocalStore::open(dir.path().join("store"), DIM).unwrap());
    let persist = std::sync::Arc::new(Persist::open(dir.path().join("memory.db")).unwrap());
    let embedder = std::sync::Arc::new(fm_embed::StubEmbedder::new(DIM));
    let engine = MemoryEngine::new(store, persist, embedder);

    let instances = build_corpus(&engine, &mut rng).await;
    let (r5, mrr) = evaluate(&engine, &instances).await;
    println!(
        "LongMemEval-subset (stub): instances={} recall@5={r5:.4} mrr={mrr:.4}",
        instances.len()
    );
    assert_meets_baseline(Baseline {
        mode: "stub".into(),
        instances: instances.len(),
        recall_at_5: r5,
        mrr,
    });
}

/// live 模式 (真 bge-m3, 需 fusion-mlx + --features mlx-live -- --include-ignored):
/// 语义质量基线, 同 never-below 规则, 基线文件 -live.json。
#[cfg(feature = "mlx-live")]
#[tokio::test]
#[ignore = "needs live fusion-mlx with bge-m3"]
async fn longmemeval_live_never_below_baseline() {
    use fm_embed::{EmbedConfig, MlxEmbedder};

    let mut rng = Rng::new(SEED);
    let dir = tempfile::tempdir().unwrap();
    let dim = 1024;
    let store = std::sync::Arc::new(LocalStore::open(dir.path().join("store"), dim).unwrap());
    let persist = std::sync::Arc::new(Persist::open(dir.path().join("memory.db")).unwrap());
    let mut ec = EmbedConfig::default();
    ec.mlx_url =
        std::env::var("FUSION_MLX_URL").unwrap_or_else(|_| "http://127.0.0.1:11434/v1".into());
    ec.embed_model = std::env::var("FUSION_MEMORY_EMBED_MODEL").unwrap_or_else(|_| "bge-m3".into());
    ec.api_key = std::env::var("FUSION_MEMORY_MLX_API_KEY").unwrap_or_else(|_| "change-me".into());
    let ec = ec.overlay_env();
    let embedder = std::sync::Arc::new(MlxEmbedder::new(ec).unwrap());
    let engine = MemoryEngine::new(store, persist, embedder);

    let instances = build_corpus(&engine, &mut rng).await;
    let (r5, mrr) = evaluate(&engine, &instances).await;
    println!(
        "LongMemEval-subset (live bge-m3): instances={} recall@5={r5:.4} mrr={mrr:.4}",
        instances.len()
    );
    // live baseline 单独入库; FM_LME_UPDATE=1 首跑生成
    let path = baseline_path()
        .to_string_lossy()
        .replace(".json", "-live.json");
    let actual = Baseline {
        mode: "live-bge-m3".into(),
        instances: instances.len(),
        recall_at_5: r5,
        mrr,
    };
    if std::env::var("FM_LME_UPDATE").as_deref() == Ok("1") {
        std::fs::write(&path, serde_json::to_string_pretty(&actual).unwrap() + "\n").unwrap();
        return;
    }
    let base: Baseline =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("live baseline JSON")).unwrap();
    assert!(
        actual.recall_at_5 >= base.recall_at_5 - 1e-9,
        "live recall@5 {} < baseline {}",
        actual.recall_at_5,
        base.recall_at_5
    );
    assert!(
        actual.mrr >= base.mrr - 1e-9,
        "live MRR {} < baseline {}",
        actual.mrr,
        base.mrr
    );
}
