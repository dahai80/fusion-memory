# Retrieval Quality Ablation — fusion scoring vs plain cosine (issue #25)

Date: 2026-10-25 · Harness: `crates/fm-engine/tests/retrieval_quality.rs` · Deterministic seed `0x5eed25abcd100001`

## Why

fusion-memory's differentiators — forgetting-curve decay, three-tier scheduling, cognitive-graph affinity, fusion scoring — were real algorithms with **zero retrieval-quality evidence** (issue #25: "无证据的信仰"). This benchmark proves or simplifies each component with recall@5 / nDCG@10 / MRR on a synthetic corpus with known ground truth.

## Corpus spec

Per topic: one basis vector (dim=64) + 2 entities joined by a `co_occurs` relation edge (in-memory SQLite `Persist`, recursive-CTE N-hop).

| Item class | Count/topic | Created | Entities | Role |
|---|---|---|---|---|
| fresh (relevant) | 5 | last 12h, access 0..3, mixed MemoryType | topic entities | ground truth |
| stale decoy | 5 | 30–60 days ago, Episodic (τ=1d → W≈0) | none | outdated near-duplicate (high cosine, wrong) |
| distract decoy | 3 | last 3 days, Semantic | wrong-topic entities | cosine confusion (0.8·b_t + 0.6·b_{t+1} mix) |

Query per topic: `0.9·b_t + noise`; every 2nd query carries a topic entity (graph arm active on 50% of queries). Fixed `now = 1_800_000_000`, fully deterministic — no wall-clock dependency.

Ranking is over all candidates (no HNSW KNN approximation) — the test isolates **scoring-formula quality**, not index recall.

## Results — CI gate scale (40 topics, 520 items, 40 queries)

| arm | recall@5 | nDCG@10 | MRR |
|---|---|---|---|
| plain cosine | 0.4800 | 0.6415 | 0.6746 |
| cosine + decay (normalized freshness) | 0.7750 | 0.9165 | 0.9625 |
| cosine + graph affinity | 0.7200 | 0.8183 | 0.8562 |
| **full fusion (α=0.5, β=0.3, γ=0.2)** | **0.8650** | **0.9548** | **0.9875** |
| fusion, raw W(t) — pre-retune, deprecated | 0.2800 | 0.4196 | 0.7952 |

Temporal validity (fresh-over-stale wins): **full fusion 40/40, plain cosine 18/40** — the structurally-CC-impossible capability works.

## Results — full scale (`FM_RQ_SCALE=full`, 400 topics, 5200 items, 400 queries)

| arm | recall@5 | nDCG@10 | MRR |
|---|---|---|---|
| plain cosine | 0.4250 | 0.5797 | 0.7100 |
| cosine + decay (normalized freshness) | 0.6325 | 0.7891 | 0.9307 |
| cosine + graph affinity | 0.7190 | 0.8016 | 0.8603 |
| **full fusion (α=0.5, β=0.3, γ=0.2)** | **0.8270** | **0.9042** | **0.9629** |
| fusion, raw W(t) — pre-retune, deprecated | 0.2285 | 0.3166 | 0.6193 |

## Finding and decision (issue acceptance #4)

**Finding**: the pre-benchmark fusion formula `0.5·cosine + 0.3·W(t) + 0.2·graph` was **worse than plain cosine** (recall@5 0.4250 → 0.2285 at full scale). Root cause: `W(t)` carries the type-dependent initial weight w0 (Episodic 0.6 / Semantic 0.8 / Procedural 1.0), so the additive β·W(t) term systematically promoted Procedural/Semantic items regardless of relevance — the decay term's scale variance, not its shape, was the defect.

**Decision: RETUNE — keep all three components and the α/β/γ weights; the decay term switches from raw `W(t)` to normalized freshness** `freshness_of()` = `exp(-t/τ(type))·reinforce` with w0 stripped, capped at 1.0 (`crates/fm-engine/src/scoring.rs`). Raw `W(t)` is unchanged and still drives the θ_drop/θ_promote scheduling thresholds (PRD §7.3) — only retrieval ranking changed.

Post-retune, full fusion beats plain cosine by **+0.40 recall@5 / +0.32 nDCG@10 / +0.25 MRR** at full scale, and the fresh-over-stale temporal win is 40/40. The complexity is now evidenced; the deprecated raw-W arm stays in the harness as the decision record.

## CI gate

`cargo test -p fm-engine --test retrieval_quality` (default scale, <1s) asserts:

1. Full fusion ≥ plain cosine on all three metrics (never-worse floor).
2. Each single-component arm ≥ plain cosine (no component drags).
3. Temporal: fusion ranks fresh above stale on ≥90% of queries and strictly more often than plain cosine.
4. Seed determinism: two runs bitwise-identical metrics.

Full-scale rerun: `FM_RQ_SCALE=full cargo test -p fm-engine --test retrieval_quality full_scale_run -- --nocapture` (~10s).
