//! Issue #25: retrieval quality evaluation — fusion scoring vs plain cosine ablation.
//!
//! 合成语料 (确定性种子) + 已知相关 ground truth, 四路消融:
//!   plain cosine | cosine+decay | cosine+graph | full fusion (α=0.5, β=0.3, γ=0.2)
//! 指标: recall@5 / nDCG@10 / MRR。
//!
//! CI gate (默认小语料, 秒级): full fusion 不得低于 plain cosine (floor) +
//! 时序有效性 (fresh 压过 stale, Claude Code 记忆结构性缺失的能力, 证明 decay 真工作)。
//! 全量规模: FM_RQ_SCALE=full (大语料, 结果落 docs/benchmarks/)。
//!
//! 排序对象是全部候选 (非 HNSW KNN 近似) — 本测试隔离评分公式质量, 不测索引召回。

use std::collections::HashMap;

use fm_core::{EntityNode, EntityType, MemoryItem, MemoryTier, MemoryType};
use fm_engine::scoring::{freshness_of, fuse_score, weight_of, GRAPH_HOP_LIMIT};
use fm_graph::affinity::affinity_from_reachability;
use fm_persist::Persist;

const SEED: u64 = 0x5eed_25ab_cd10_0001;
const DIM: usize = 64;
/// 固定 "now" (秒)。全确定性, 不依赖真实时钟。
const NOW: u64 = 1_800_000_000;
const HOUR: u64 = 3600;
const DAY: u64 = 86_400;

/// 确定性 xorshift64* RNG。零外部依赖, 固定种子可复现。
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        // 0 种子非法, 混入常数
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

    /// [0, 1) 均匀
    fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// 近似正态 N(0,1): 4 个均匀和 - 2 (Irwin-Hall n=4, 方差 1/3 → 校正)。
    fn normal(&mut self) -> f64 {
        let s: f64 = (0..4).map(|_| self.next_f64()).sum::<f64>() - 2.0;
        s * 1.7320508 // sqrt(3), 校正方差到 1
    }
}

/// 单条候选: item 元数据 + 向量 (向量不入库, 排序直接用)。
struct Candidate {
    item: MemoryItem,
    vector: Vec<f32>,
}

struct Query {
    vector: Vec<f32>,
    entity_ids: Vec<String>,
    relevant: Vec<String>,
    topic: usize,
}

struct Corpus {
    candidates: Vec<Candidate>,
    queries: Vec<Query>,
    /// topic -> (fresh ids, stale ids) — 时序有效性检查用
    temporal: Vec<(Vec<String>, Vec<String>)>,
    persist: Persist,
}

struct CorpusParams {
    n_topics: usize,
    fresh_per_topic: usize,
    stale_per_topic: usize,
    distract_per_topic: usize,
    /// 带 query 实体的主题占比 (其余 query 无实体, graph 臂只作用这部分)
    entity_query_ratio: usize,
}

fn gate_params() -> CorpusParams {
    CorpusParams {
        n_topics: 40,
        fresh_per_topic: 5,
        stale_per_topic: 5,
        distract_per_topic: 3,
        // 每隔 2 个主题 1 个带实体 query → 50% query 携带实体
        entity_query_ratio: 2,
    }
}

fn full_params() -> CorpusParams {
    CorpusParams {
        n_topics: 400,
        fresh_per_topic: 5,
        stale_per_topic: 5,
        distract_per_topic: 3,
        entity_query_ratio: 2,
    }
}

/// 单位随机向量
fn unit_vec(rng: &mut Rng) -> Vec<f32> {
    let v: Vec<f32> = (0..DIM).map(|_| rng.normal() as f32).collect();
    let n = (v.iter().map(|x| x * x).sum::<f32>()).sqrt();
    v.iter().map(|x| x / n.max(1e-9)).collect()
}

/// 归一化
fn normalize(v: &[f32]) -> Vec<f32> {
    let n = (v.iter().map(|x| x * x).sum::<f32>()).sqrt();
    v.iter().map(|x| x / n.max(1e-9)).collect()
}

fn mk_item(
    id: &str,
    mt: MemoryType,
    created: u64,
    access: u64,
    entities: Vec<EntityNode>,
) -> MemoryItem {
    let mut m = MemoryItem::new_turn_skeleton(
        id.into(),
        "rq-bench".into(),
        0,
        "rq-session".into(),
        String::new(),
        mt,
        format!("content of {id}"),
        created,
    );
    m.tier = MemoryTier::Short;
    m.weight = mt.initial_weight();
    m.access_count = access;
    m.last_accessed_timestamp = created;
    m.entities = entities;
    m
}

/// 构造合成语料。
///
/// 主题 t: 基向量 b_t + 2 实体 (e1_t, e2_t, 库内 relation 边 e1_t-e2_t)。
///   fresh (相关): created 近 12h, 带主题实体, access 0..3
///   stale (干扰): 同主题同词 (高 cosine) 但 created 30-60 天前 (过时版本), 无实体
///   distract (干扰): 0.8*b_t + 0.6*b_(t+1) 混合 (cosine 混淆), 实体来自无关主题
/// query: 0.9*b_t + 噪声; 相关 = fresh ids; 偶数主题 query 携带主题实体。
fn build_corpus(p: &CorpusParams) -> Corpus {
    let mut rng = Rng::new(SEED);
    let persist = Persist::open_in_memory().expect("in-memory persist");

    let mut candidates: Vec<Candidate> = Vec::new();
    let mut queries: Vec<Query> = Vec::new();
    let mut temporal: Vec<(Vec<String>, Vec<String>)> = Vec::new();

    // 主题基向量 + 实体 + 图边
    let mut bases = Vec::with_capacity(p.n_topics);
    let mut topic_entities = Vec::with_capacity(p.n_topics);
    for t in 0..p.n_topics {
        bases.push(unit_vec(&mut rng));
        let e1 = format!("ent-{t}-a");
        let e2 = format!("ent-{t}-b");
        topic_entities.push((e1.clone(), e2.clone()));
        // 主题内实体关系边 (graph 亲和来源)
        persist
            .put_relation(&e1, &e2, "co_occurs", 1.0, 1, NOW)
            .expect("put relation");
    }

    let mut id_seq = 0u64;
    let mut next_id = |prefix: &str| {
        id_seq += 1;
        format!("mem-{prefix}-{id_seq}")
    };

    let types = [
        MemoryType::Episodic,
        MemoryType::Semantic,
        MemoryType::Procedural,
    ];

    for t in 0..p.n_topics {
        let base = &bases[t];
        let mut fresh_ids = Vec::new();
        let mut stale_ids = Vec::new();

        // fresh 相关: 近 12h 创建, 带主题实体
        for _ in 0..p.fresh_per_topic {
            let id = next_id("fresh");
            let noise: Vec<f32> = (0..DIM).map(|_| rng.normal() as f32 * 0.15).collect();
            let v = normalize(
                base.iter()
                    .zip(noise.iter())
                    .map(|(b, n)| b + n)
                    .collect::<Vec<f32>>()
                    .as_slice(),
            );
            let created = NOW - (rng.next_f64() * 12.0 * HOUR as f64) as u64;
            let access = rng.next_u64() % 4;
            let mt = types[rng.next_u64() as usize % types.len()];
            let (e1, e2) = &topic_entities[t];
            let entities = vec![
                EntityNode::new(e1.clone(), e1.clone(), EntityType::Tech),
                EntityNode::new(e2.clone(), e2.clone(), EntityType::Tech),
            ];
            candidates.push(Candidate {
                item: mk_item(&id, mt, created, access, entities),
                vector: v,
            });
            fresh_ids.push(id);
        }

        // stale 干扰: 同主题同词 (高 cosine), 30-60 天前 (过时版本), 无实体
        for _ in 0..p.stale_per_topic {
            let id = next_id("stale");
            let noise: Vec<f32> = (0..DIM).map(|_| rng.normal() as f32 * 0.15).collect();
            let v = normalize(
                base.iter()
                    .zip(noise.iter())
                    .map(|(b, n)| b + n)
                    .collect::<Vec<f32>>()
                    .as_slice(),
            );
            let created = NOW - (30.0 * DAY as f64 + rng.next_f64() * 30.0 * DAY as f64) as u64;
            let access = rng.next_u64() % 2;
            candidates.push(Candidate {
                // 过时条目多为 Episodic (τ=1d, 衰减最狠)
                item: mk_item(&id, MemoryType::Episodic, created, access, Vec::new()),
                vector: v,
            });
            stale_ids.push(id);
        }

        // distract 干扰: 主题 t 与 t+1 混词 (cosine 混淆), 实体来自无关主题 (t+2)
        for _ in 0..p.distract_per_topic {
            let id = next_id("dist");
            let other = &bases[(t + 1) % p.n_topics];
            let mixed: Vec<f32> = base
                .iter()
                .zip(other.iter())
                .map(|(a, b)| 0.8 * a + 0.6 * b)
                .collect();
            let noise: Vec<f32> = (0..DIM).map(|_| rng.normal() as f32 * 0.15).collect();
            let v = normalize(
                mixed
                    .iter()
                    .zip(noise.iter())
                    .map(|(m, n)| m + n)
                    .collect::<Vec<f32>>()
                    .as_slice(),
            );
            let created = NOW - (rng.next_f64() * 3.0 * DAY as f64) as u64;
            let wrong_topic = (t + 2) % p.n_topics;
            let (e1, _) = &topic_entities[wrong_topic];
            let entities = vec![EntityNode::new(e1.clone(), e1.clone(), EntityType::Tech)];
            candidates.push(Candidate {
                item: mk_item(&id, MemoryType::Semantic, created, 0, entities),
                vector: v,
            });
        }

        temporal.push((fresh_ids.clone(), stale_ids));

        // query: 0.9*b_t + 噪声; 相关 = fresh; 偶数主题带实体
        let qnoise: Vec<f32> = (0..DIM).map(|_| rng.normal() as f32 * 0.12).collect();
        let qv = normalize(
            base.iter()
                .zip(qnoise.iter())
                .map(|(b, n)| 0.9 * b + n)
                .collect::<Vec<f32>>()
                .as_slice(),
        );
        let entity_ids = if t % p.entity_query_ratio == 0 {
            let (e1, _) = &topic_entities[t];
            vec![e1.clone()]
        } else {
            Vec::new()
        };
        queries.push(Query {
            vector: qv,
            entity_ids,
            relevant: fresh_ids,
            topic: t,
        });
    }

    Corpus {
        candidates,
        queries,
        temporal,
        persist,
    }
}

// ---- 评分臂 ----

#[derive(Clone, Copy, PartialEq, Debug)]
enum Arm {
    PlainCosine,
    CosineDecay,
    CosineGraph,
    FullFusion,
    /// #25 裁定证据: retune 前的原始 W(t) 融合 (含 w0 类型差), 已弃用 — 仅作对照记录。
    FusionRawW,
}

impl Arm {
    fn name(self) -> &'static str {
        match self {
            Arm::PlainCosine => "plain cosine",
            Arm::CosineDecay => "cosine+decay",
            Arm::CosineGraph => "cosine+graph",
            Arm::FullFusion => "full fusion",
            Arm::FusionRawW => "fusion raw-W (pre-retune, deprecated)",
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct Metrics {
    recall5: f64,
    ndcg10: f64,
    mrr: f64,
}

/// 排序: 全候选打分 desc, 同分按 id 字典序 (确定性)。
fn rank_query(
    corpus: &Corpus,
    q: &Query,
    arm: Arm,
    reachable_cache: &HashMap<String, Vec<(String, usize)>>,
) -> Vec<String> {
    // graph 臂: 预计算 query 实体可达集 (cache 跨候选复用)
    let mut scored: Vec<(f64, &str)> = corpus
        .candidates
        .iter()
        .map(|c| {
            let cos = fm_similarity::cosine(&q.vector, &c.vector).unwrap_or(0.0);
            // #25 retune 后融合用归一化新鲜度; raw-W 臂保留 W(t) 原值作对照
            let w = match arm {
                Arm::FusionRawW => weight_of(&c.item, NOW),
                _ => freshness_of(&c.item, NOW),
            };
            let aff = match arm {
                Arm::PlainCosine | Arm::CosineDecay => 0.0,
                _ => affinity_for(&q.entity_ids, &c.item, reachable_cache),
            };
            let score = match arm {
                Arm::PlainCosine => cos,
                Arm::CosineDecay => fuse_score(cos, w, 0.0),
                Arm::CosineGraph => fuse_score(cos, 0.0, aff),
                Arm::FullFusion | Arm::FusionRawW => fuse_score(cos, w, aff),
            };
            (score, c.item.id.as_str())
        })
        .collect();
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap().then(a.1.cmp(b.1)));
    scored.into_iter().map(|(_, id)| id.to_string()).collect()
}

/// graph_affinity 语义复刻 (fm_graph::graph_affinity 同逻辑, 可达集走 cache):
/// 直命中 1.0; 可达集最短跳 0.5^h; 否则 0。
fn affinity_for(
    query_ids: &[String],
    cand: &MemoryItem,
    reachable_cache: &HashMap<String, Vec<(String, usize)>>,
) -> f64 {
    let cand_ids: Vec<String> = fm_engine::scoring::entity_ids(&cand.entities);
    if query_ids.is_empty() || cand_ids.is_empty() {
        return 0.0;
    }
    let mut best = 0.0f64;
    for q in query_ids {
        // 直命中
        if cand_ids.iter().any(|c| c == q) {
            return 1.0;
        }
        if let Some(reachable) = reachable_cache.get(q) {
            let aff = affinity_from_reachability(std::slice::from_ref(q), &cand_ids, reachable);
            if aff > best {
                best = aff;
            }
        }
    }
    best
}

/// 预计算所有 query 实体的 N-hop 可达集 (每实体一次 CTE, 候选复用)。
fn build_reachable_cache(corpus: &Corpus) -> HashMap<String, Vec<(String, usize)>> {
    let mut cache = HashMap::new();
    for q in &corpus.queries {
        for e in &q.entity_ids {
            if !cache.contains_key(e) {
                let r = corpus
                    .persist
                    .n_hop_reachable(e, GRAPH_HOP_LIMIT)
                    .expect("n_hop");
                cache.insert(e.clone(), r);
            }
        }
    }
    cache
}

fn evaluate(corpus: &Corpus, arm: Arm, cache: &HashMap<String, Vec<(String, usize)>>) -> Metrics {
    let mut recall_sum = 0.0f64;
    let mut ndcg_sum = 0.0f64;
    let mut mrr_sum = 0.0f64;
    let n = corpus.queries.len() as f64;

    for q in &corpus.queries {
        let ranked = rank_query(corpus, q, arm, cache);
        let rel: std::collections::HashSet<&str> = q.relevant.iter().map(|s| s.as_str()).collect();

        // recall@5
        let hits5 = ranked
            .iter()
            .take(5)
            .filter(|id| rel.contains(id.as_str()))
            .count();
        recall_sum += hits5 as f64 / q.relevant.len() as f64;

        // nDCG@10 (二值增益)
        let mut dcg = 0.0f64;
        for (i, id) in ranked.iter().take(10).enumerate() {
            if rel.contains(id.as_str()) {
                dcg += 1.0 / ((i + 2) as f64).log2(); // 1/log2(rank+1), rank 从 1
            }
        }
        let ideal = q.relevant.len().min(10) as f64;
        let idcg: f64 = (1..=ideal as usize)
            .map(|i| 1.0 / ((i + 1) as f64).log2())
            .sum();
        ndcg_sum += if idcg > 0.0 { dcg / idcg } else { 0.0 };

        // MRR
        if let Some(rank) = ranked.iter().position(|id| rel.contains(id.as_str())) {
            mrr_sum += 1.0 / (rank + 1) as f64;
        }
    }

    Metrics {
        recall5: recall_sum / n,
        ndcg10: ndcg_sum / n,
        mrr: mrr_sum / n,
    }
}

fn run_ablation(params: &CorpusParams) -> (Vec<(Arm, Metrics)>, Corpus) {
    let corpus = build_corpus(params);
    let cache = build_reachable_cache(&corpus);
    let arms = [
        Arm::PlainCosine,
        Arm::CosineDecay,
        Arm::CosineGraph,
        Arm::FullFusion,
        Arm::FusionRawW,
    ];
    let results: Vec<(Arm, Metrics)> = arms
        .iter()
        .map(|a| (*a, evaluate(&corpus, *a, &cache)))
        .collect();
    (results, corpus)
}

fn print_table(params: &CorpusParams, results: &[(Arm, Metrics)]) {
    let n_items = params.n_topics
        * (params.fresh_per_topic + params.stale_per_topic + params.distract_per_topic);
    println!("\n=== retrieval quality ablation (issue #25) ===");
    println!(
        "corpus: {} topics x ({} fresh + {} stale + {} distract) = {} items, {} queries, dim={}, seed={:#x}, NOW={}",
        params.n_topics, params.fresh_per_topic, params.stale_per_topic, params.distract_per_topic,
        n_items, params.n_topics, DIM, SEED, NOW
    );
    println!(
        "{:<16} {:>10} {:>10} {:>10}",
        "arm", "recall@5", "nDCG@10", "MRR"
    );
    for (arm, m) in results {
        println!(
            "{:<16} {:>10.4} {:>10.4} {:>10.4}",
            arm.name(),
            m.recall5,
            m.ndcg10,
            m.mrr
        );
    }
}

fn metrics_of(results: &[(Arm, Metrics)], arm: Arm) -> Metrics {
    results.iter().find(|(a, _)| *a == arm).unwrap().1
}

// ---- 测试 ----

/// 全四臂消融 + gate: full fusion 不得低于 plain cosine (floor, issue 验收 #3)。
/// 打印指标表 (cargo test -- --nocapture 可见)。
#[test]
fn ablation_full_fusion_never_below_plain_cosine() {
    let params = gate_params();
    let (results, _corpus) = run_ablation(&params);
    print_table(&params, &results);

    let plain = metrics_of(&results, Arm::PlainCosine);
    let full = metrics_of(&results, Arm::FullFusion);
    let decay = metrics_of(&results, Arm::CosineDecay);
    let graph = metrics_of(&results, Arm::CosineGraph);

    // floor: fusion 永不更差 (三指标)
    assert!(
        full.recall5 >= plain.recall5 - 1e-9,
        "full recall@5 {full:?} < plain {plain:?}"
    );
    assert!(
        full.ndcg10 >= plain.ndcg10 - 1e-9,
        "full nDCG@10 {full:?} < plain {plain:?}"
    );
    assert!(
        full.mrr >= plain.mrr - 1e-9,
        "full MRR {full:?} < plain {plain:?}"
    );

    // 单臂各自也应在 floor 上 (组件单独不拖后腿)
    assert!(
        decay.recall5 >= plain.recall5 - 1e-9,
        "decay 臂拖后腿: {decay:?} vs {plain:?}"
    );
    assert!(
        graph.recall5 >= plain.recall5 - 1e-9,
        "graph 臂拖后腿: {graph:?} vs {plain:?}"
    );
}

/// 时序有效性: 同主题 fresh vs stale 竞争, decay 应把 fresh 排前面。
/// 这是 Claude Code 式 "本地文件记忆" 结构性缺失的能力 (issue 验收 #4 前半)。
#[test]
fn temporal_validity_fresh_beats_stale() {
    let params = gate_params();
    let (results, corpus) = run_ablation(&params);
    let cache = build_reachable_cache(&corpus);

    let mut fusion_wins = 0usize;
    let mut plain_wins = 0usize;
    for q in &corpus.queries {
        let (fresh, stale) = &corpus.temporal[q.topic];
        let check = |arm: Arm| -> bool {
            let ranked = rank_query(&corpus, q, arm, &cache);
            let pos = |ids: &Vec<String>| -> usize {
                ids.iter()
                    .map(|id| ranked.iter().position(|r| r == id).unwrap_or(usize::MAX))
                    .min()
                    .unwrap_or(usize::MAX)
            };
            pos(fresh) < pos(stale)
        };
        if check(Arm::FullFusion) {
            fusion_wins += 1;
        }
        if check(Arm::PlainCosine) {
            plain_wins += 1;
        }
    }
    let n = corpus.queries.len();
    println!(
        "\ntemporal: fresh-over-stale wins — fusion {fusion_wins}/{n}, plain cosine {plain_wins}/{n}"
    );

    // decay 应让 fusion 在绝大多数 query 上 fresh 压过 stale (噪声同分布下 plain ~50%)
    assert!(
        fusion_wins as f64 / n as f64 >= 0.9,
        "fusion fresh-beats-stale 只有 {fusion_wins}/{n}, decay 未生效"
    );
    // 且显著优于 plain cosine (证明 decay 增益非噪声)
    assert!(
        fusion_wins > plain_wins,
        "fusion {fusion_wins} 未超 plain {plain_wins}"
    );

    // full fusion 指标优于 plain (此处 corpus 上应严格更好; 弱化为非劣已在 floor test 保证)
    let plain = metrics_of(&results, Arm::PlainCosine);
    let full = metrics_of(&results, Arm::FullFusion);
    assert!(
        full.recall5 > plain.recall5,
        "recall@5 full {full:?} 未超 plain {plain:?}"
    );
    assert!(
        full.ndcg10 > plain.ndcg10,
        "nDCG@10 full {full:?} 未超 plain {plain:?}"
    );
}

/// 确定性: 同参数两次构建, 指标逐位一致。
#[test]
fn deterministic_seed() {
    let p = gate_params();
    let (r1, _) = run_ablation(&p);
    let (r2, _) = run_ablation(&p);
    for ((a1, m1), (_, m2)) in r1.iter().zip(r2.iter()) {
        assert!((m1.recall5 - m2.recall5).abs() < 1e-12, "arm {a1:?} 不确定");
        assert!((m1.ndcg10 - m2.ndcg10).abs() < 1e-12, "arm {a1:?} 不确定");
        assert!((m1.mrr - m2.mrr).abs() < 1e-12, "arm {a1:?} 不确定");
    }
}

/// 全量规模 (FM_RQ_SCALE=full 时跑大语料, 结果人工落 docs/benchmarks/)。
/// 默认仍跑 gate 规模 — CI 时长约束 (<2min)。
#[test]
fn full_scale_run() {
    let params = if std::env::var("FM_RQ_SCALE").as_deref() == Ok("full") {
        full_params()
    } else {
        gate_params()
    };
    let (results, _corpus) = run_ablation(&params);
    print_table(&params, &results);
    // floor 在全量规模同样成立
    let plain = metrics_of(&results, Arm::PlainCosine);
    let full = metrics_of(&results, Arm::FullFusion);
    assert!(
        full.recall5 >= plain.recall5 - 1e-9,
        "full scale floor broken"
    );
}
