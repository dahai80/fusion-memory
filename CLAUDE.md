---
doc_id: fusion-memory/guide
product: fusion-memory
class: core
entry: service
doc_type: guide
title: { zh: "Claude Code 工作指引", en: "Claude Code guidance" }
audience: engineer
status: draft
version: 1.6.0
updated: 2026-11-07
owner: "@dahai"
source_of_truth:
  - CLAUDE.md
---

# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

`fusion-memory` is the **system-level long/short-term memory and cognitive graph hub** of the Fusion ecosystem ("一核九端"). It exists to close the cross-session state gap that plagues Agents: repeated questions, lost context, and context-window blowup. The end goal is an Agent that knows the user better the more it is used.

Authoritative spec: `architecture/fusion-memory-prd-0825.md` (repo root `architecture/`).

## Current Status

**Built — M0 through M6 landed. v1.9.0 scale-and-finish released (evolution plan 2026-11 phase 4, issues #51/#52/#53): M9 cluster auto-failover activation — `spawn_cluster_with` DI variant (per-node ElectionConfig injection; NODE_ID process-global env), split-vote livelock fix (deterministic campaign stagger self_id×250ms + lease-bounded recently-voted-skip + sitting-leader vote deny), 3-node live e2e `crates/fm-server/tests/cluster_failover_3node.rs` (leader sync replicates to both followers over real TCP → live leader killed → quorum promotion role file + epoch++ → promoted node serves new epoch, fresh follower receives post-failover commit); known limitation: promoted follower-leader wop log holds post-promotion writes only (replayed rows bypass wop), snapshot/compaction future work; M10 fm-py deprecated (zero code consumers repo-wide, service surface = contract, kept compiling under API freeze, HTTP migration path in crate docs); governance panel — `config` RPC (UDS+HTTP `POST /v1/memory/config`) via `fm_engine::scoring::governance_snapshot()` + web Governance tab + Audit trust-event filter. 552 offline tests (558 store-fusion). v1.8.0 skills-and-export released (evolution plan 2026-11 phase 3, issues #48/#49): M7 SKILL.md export — Procedural memories render as skills-contract docs (YAML frontmatter name-from-first-entity/description-first-line-160/version/source: fusion-memory/license: Apache-2.0 + redacted body + Provenance footer), deterministic template no model call (Rule 5), `export_skills` RPC (UDS+HTTP, limit 1..=50, namespace scoping, ACL owner filter); M8 revisions+export — schema v9 `memory_revision` (rev PK AUTOINCREMENT, `CREATE IF NOT EXISTS` idempotent no-ALTER), `maybe_record_revision` snapshots PRIOR encrypted content on actual change only (idempotent replay safe), `list_revisions` additive trait method (default Unsupported) + `revisions` RPC (subject invisible → 404, ACL other-owner history → 404), merge write-back records via same put path; `export` RPC `format jsonl|markdown` (limit 1..=1000, JSONL serde round-trippable with vector_ref blanked, Markdown wikilink projection `[[memory-id]]`/`[[entity-name]]`); defense line 2 — all renderers apply `redact_text()` unconditionally (`FUSION_MEMORY_REDACT_PII=0` ingestion-only, never egress), e2e redact assertion in `crates/fm-server/tests/skill_export.rs` (phone/email never raw in any exported body) + agent-studio consumption contract (frontmatter shape). 551 offline tests (557 store-fusion). v1.7.0 time-and-trust released (evolution plan 2026-11 phase 2, issues #44/#45/#46): M3 validity windows — `valid_from`/`valid_until` (schema v6, epoch ms via free-form `interaction.metadata`, zero wire-contract change), `validity_of()` no-window 1.0 / not-yet-valid 0.0 / expired 0.1 (downweight, never delete), score = fuse_score4 x validity multiplicative stacking (defense line 3), ablation gate `validity_window_ablation_never_below` (expired freshness-heavy item wins 40/40 without windows, loses 40/40 with them, window-free items bit-identical); M6 trust chain — `fm-engine::trust` LocalScanner (INJ-001..007 ported from fusion-security, always-on offline fail-safe floor) + HttpScanner (opt-in loopback-only `FUSION_MEMORY_INJECTION_URL`/`FUSION_MEMORY_INJECTION_TOKEN`, non-loopback refuses to start) + `scan_with_fallback` (rule_id dedup, outage degrades to local), commit/summarize/merge injection gates (poisoned inputs audited `trust/injection-blocked`/`injection-skipped`, excluded from distillation), consolidation products carry `confidence` (0.5/0.75/0.95 corroboration ladder) + `sources` episodes, `provenance` RPC (UDS+HTTP) + web Provenance tab = poisoning governance surface, upstream fusion-security#85; M5 per-memory ACL — identity from the v1.3.0 fusion-identity JWT boundary (`sub`/`role` serde-default extraction, empty = fail-closed), `FUSION_MEMORY_ACL` requires multi_tenant (validate refuses otherwise — no bypass identity channel, tenant still gateway-stamp-only, defense line 5), commit stamps owner authoritatively from JWT sub, boundary enforcement (UDS trusted, engine identity-agnostic): get/delete/provenance deny NotFound, retrieve/list filter, admin bypass, shared rows visible; schema v7 (confidence REAL + sources_json) and v8 (owner TEXT) incremental guarded migrations; 547 offline tests with --features fm-store/store-fusion). v1.6.0 retrieval+consolidation released (tag `v1.6.0`, evolution plan 2026-11 phase 1, issues #39/#40/#41): M1 four-way hybrid retrieval — FTS5 trigram keyword index (schema v5 `memory_fts`) fused with cosine/graph/freshness at 0.4/0.3/0.2/0.1 (`fuse_score4`), cipher deployments keep no plaintext token index (B-1 airtight), ablation full-scale recall@5 cosine 0.2496 → three-way 0.5443 → four-way 0.6779 with CI never-below gates; M2 dreaming scheduler — idle-triggered background consolidation in fm-server (in-flight gate + pending threshold, same `consolidate_memories()` entry, saga kernel untouched, audit_log boundary events), default off via `FUSION_MEMORY_DREAM_INTERVAL_SECS`/`FUSION_MEMORY_DREAM_PENDING_THRESHOLD`; M4 LongMemEval-subset CI gate — dual embedder mode (stub/live bge-m3), committed baseline JSONs + never-below, both modes recall@5 1.0/MRR 1.0. v1.5.1 docs-only patch released (tag `v1.5.1`, PR #36 docs-system adoption — fusion docs-system v1.1 contract (core x service): `docs/` page set `product-page`/`get-started`/`architecture`/`api-reference` (adds `GET /healthz`, `/metrics`, `GET /v1/memory/version`)/`config-reference`/`troubleshooting` under the frontmatter contract (`doc_id`/`doc_type`/`status`/`source_of_truth`), `ci/validate_docs.py` docs-contract linter + `.github/workflows/docs.yml` (14 docs scanned, 0 errors), `AGENTS.md` agent guide; `CLAUDE.md`/`README.md`/`README_CN.md`/`clients/README.md`/`deploy/README.md`/`CHANGELOG.md` gain frontmatter headers, content semantics unchanged; zero Rust code/API change, crate versions bumped for release bookkeeping only); v1.5.0 minor released (tag `v1.5.0`, issue #33 web surface — browse/manage REST APIs + embedded web console: five additive RPC methods `list`/`audit_log`/`merges`/`unmerge`/`reconcile` on UDS+HTTP same dispatch (`list` limit 1..=200 with memory_type/namespace/include_tombstoned filters, `audit_log` limit 1..=500, unmerge/reconcile require `confirm=true` B5); backed by five additive `FusionMemoryEngine` trait methods with default `Unsupported` impls (v1.2.0 tenant pattern), browse DTOs in `fm_core::browse`; `list` filters tombstoned rows in memory for tenant scoping (upstream `list_tombstoned` has no SQL tenant filter); web console `GET /app` + `/app/*path` — React 18 + Vite + Tailwind v4 + lucide-react dark zinc/indigo, source `crates/fm-server/web/`, built dist embedded at compile time via `include_dir` (no runtime files, CI needs no bun), static shell public but all data through Bearer `/v1/*`; HTTP-only import route `POST /v1/memory/import` (body IS the params object `{"source":...}`, default `~/.fusion-agent-studio/memory.db`, returns bare report; reuses the server concrete engine — a second open hit the sled exclusive lock; implementation moved fm-cli → `fm_engine::import_studio`, CLI + HTTP share one impl); deviation: chunked import progress streaming deferred, sync final report); v1.4.5 patch released (tag `v1.4.5`, issue #31 config governance Phase 3.3 — start.sh registry self-check: after the port-in-use check `start.sh start` verifies its own `port-registry.yaml` entry (`FUSION_REGISTRY`, default `~/fusion/architecture/port-registry.yaml`) and refuses to start on an unregistered port (exit 1), moving the CI check-ports failure forward to first boot; missing registry file = warn + continue for standalone deploys, `FUSION_SKIP_REGISTRY_CHECK=1` bypasses; shell-only patch, no Rust code/API change, acceptance verified against temp registry copies across all four paths); v1.4.4 patch released (tag `v1.4.4`, issue #29 config governance Phase 2.2 — env contract documentation + check-env false-positive fix: issue's required-rename list for this repo was EMPTY (all names already `FUSION_MEMORY_*` per conventions §1); the lone `[legacy] start.sh` finding was a checker substring false positive (compliant `FUSION_MEMORY_MLX_API_KEY` ⊃ `MLX_API_KEY`), fixed in workspace `scripts/check-env.py` rule 2c to exact-token prefix matching (alias variants like `FUSION_MLX_API_KEY_FILE` still caught, `FUSION_FUSION_` keeps substring per rule 2a; workspace warns 136→132, 4 dropped findings diff-verified false positives); repo deliverable = consolidated "Environment variables" section in README/README_CN — full `FUSION_MEMORY_*` inventory with defaults + env > TOML > secret-file > default priority; docs-only, zero code/API change; acceptance `python scripts/check-env.py` 0 findings); v1.4.3 patch released (tag `v1.4.3`, issue #27 CI-red fix — cross-repo path deps `fg-redact`/`fs-core` broke CI standalone checkout (workspace manifest failed to load before any build step; v1.4.2 landed red 3 runs); both replaced with pinned git tag deps on public upstreams `dahai80/fusion-guard` `v0.2.0-rc.7` + `dahai80/fusion-store` `v0.2.0-rc.3`, zero drift vs local trees; verified by full `cargo test --workspace` in a fresh lone clone with no sibling repos, retrieval-quality gate now actually runs in CI; runtime stays 100% offline, build-time cargo git fetch cached in `~/.cargo/git`, local co-dev via `[patch."https://github.com/dahai80/fusion-guard"]`); v1.4.2 patch released (tag `v1.4.2`, issue #25 retrieval quality ablation — `fm-engine/tests/retrieval_quality.rs` recall@5/nDCG@10/MRR vs plain cosine, CI gate fusion-never-below-cosine + temporal fresh-over-stale + seed determinism; finding: raw-`W(t)` fusion was WORSE than plain cosine (recall@5 0.4250→0.2285 full scale, w0 type-variance defect) → retuned fusion decay term to `freshness_of()` normalized freshness (w0 stripped, capped 1.0), α/β/γ + θ_drop/θ_promote thresholds unchanged; post-retune full fusion beats plain cosine +0.40 recall@5, data in `docs/benchmarks/retrieval-quality-2026-10-25.md`); v1.4.1 patch released (tag `v1.4.1`, issue #23 `[embed]` TOML mirror for the MLX embedding API key: `mlx_url`/`embed_model`/`mlx_api_key`/`mlx_api_key_file` + env `FUSION_MEMORY_MLX_API_KEY_FILE`, env still wins, embedder + extractor share one resolved config); v1.4.0 released (tag `v1.4.0`, issue #21 scoped memory namespaces); v1.3.0 fusion-identity integration (tag `v1.3.0`, issue #18); v1.2.0 multi-tenant isolation (tag `v1.2.0`, issue #16); v1.1.0 Commercial GA released (tag `v1.1.0`).** Rust Cargo workspace, 12 crates (`fm-cli`/`fm-core`/`fm-embed`/`fm-engine`/`fm-fuzz`/`fm-graph`/`fm-persist`/`fm-py`/`fm-server`/`fm-similarity`/`fm-store`/`fm-cluster`). 515 offline tests green (521 with `--features fm-store/store-fusion`), regions 90.82% offline / 92.47% live. Milestone detail + PRD deviation records in `README.md`. v1.4.0: scoped memory namespaces (tenant-orthogonal visibility tiers `user`/`project`/`local` on commit/retrieve/consolidate; `namespace`/`namespace_key` data fields with `#[serde(default)]`; stacking project ⊂ user, local machine-only; schema v4 + `idx_memory_namespace`; scoped persist queries; `consolidate_memories_scoped` does not advance the global watermark; optional RPC params on commit/retrieve/memory.retrieve_context/consolidate; invalid namespace → -32602 fail-closed; consumer side in fusion-agent-studio#434). v1.3.0: fusion-identity integration (fm-server HTTP boundary verifies caller JWT via `POST /api/v1/auth/verify`, enforces tid↔tenant match → 403 on mismatch, rejects empty tenant in multi-tenant mode → 401 fail-closed, reports usage; `multi_tenant` config default false = single-tenant backward compat, no identity verify; `IdentityVerifier` trait + `RealVerifier`/`NoopVerifier` for 100% offline testability; config `multi_tenant`/`identity_url`/`identity_service_token` + env `FUSION_MEMORY_MULTI_TENANT`/`FUSION_MEMORY_IDENTITY_URL`/`FUSION_MEMORY_IDENTITY_SERVICE_TOKEN`). v1.2.0: multi-tenant isolation (tenant field on data, additive tenant-scoped trait methods, schema v3, gateway-origin enforcement `X-Fusion-Route`/`X-Fusion-Tenant`, config `gateway_origin_required`/`default_tenant`). RC known limitations all closed in 1.1.0: live perf baseline (real bge-m3), store-fusion adapter, fg-redact credentials. Remaining non-blocking: M5 store-fusion switch + guard DLP gate degraded (see README "M5 PRD 偏离记录"); PII stays fusion-memory-local pending fusion-guard #13. M4 outward integration PRs: `fusion-cowork` #68 + `fusion-agent-studio` #247 merged; `fusion-code` #151 merged / #156 open.

## Architecture (from PRD)

### Tech Selection

| Module | PRD plan | Actual implementation | Why diverged |
|--------|----------|----------------------|--------------|
| Core language | Rust | Rust (edition 2021, MSRV 1.87) | as planned |
| Graph storage | SQLite + Kuzu DB | SQLite + recursive CTE (`WITH RECURSIVE` N-hop) | Kuzu has no Rust binding (Rule 7, see README "M2 PRD 偏离记录") |
| Vector retrieval | `fusion-store` (HNSW zero-copy) | `store-stub` (hnsw_rs + sled) | fusion-store trait API mismatch + cross-project constraint + A4 denied zero-copy (see README "M5 PRD 偏离记录"); store-stub is long-term production backend |
| Embedding | (M2) fusion-mlx bge-m3 | `fm-embed` MlxEmbedder (bge-m3, dim=1024) + StubEmbedder (FNV-1a, offline) | as planned |
| Memory scoring | NEON SIMD | Rust `f32` cosine in `fm-similarity` | Rule 2 simplicity; PRD SIMD was speculative |

### Business Boundary

**In-scope:**
- Cross-session dialogue/behavior analysis and Entity extraction
- Three-tier memory scheduling: Working Memory → Short-Term → Long-Term Graph
- Forgetting-curve decay algorithm fused with Knowledge Graph Alignment

**Out-of-scope:**
- Raw code AST parsing → belongs to `fusion-rag`
- Low-level file and vector page storage management → belongs to `fusion-store`

### Ecosystem Layer

`fusion-memory` sits in the **Governance & Context Layer** alongside `fusion-guard` (TCC/DLP) and `fusion-rag` (AST). Above it: the Perception & Execution Layer (`fusion-executor`, `fusion-browser`, `fusion-event`). Below it: the Infrastructure Layer (`fusion-core`, `fusion-store`, `fusion-mlx`, `fusion-gateway`). Components communicate via Unix Domain Socket and Metal shared memory (zero-copy, offline).

### Core Data Structures & API (PRD-defined)

```rust
pub struct MemoryItem {
    pub id: String,
    pub memory_type: MemoryType, // Episodic, Semantic, Procedural
    pub content: String,
    pub entities: Vec<EntityNode>,
    pub vector_ref: u64,
    pub weight: f32,             // initial weight
    pub last_accessed_timestamp: u64,
}

pub trait FusionMemoryEngine {
    // Commit a memory fragment and auto-distill knowledge-graph nodes
    fn commit_episodic_memory(&self, session_id: &str, interaction: &Interaction) -> Result<MemoryId>;
    // Dynamically retrieve and assemble memory context for the current prompt (post-compression)
    fn retrieve_context(&self, query_vector: &[f32], top_k: usize) -> FormattedContext;
    // Trigger background forgetting + consolidation (nightly cron process)
    fn consolidate_memories(&self) -> ConsolidationReport;
}
```

## Build, Test, Lint

Follows the monorepo Rust pattern (see `fusion-cli` / `fusion-design`):

```bash
cargo check --workspace                            # Compile check
cargo test --workspace                             # All offline tests (515 cases, excludes fm-py cdylib; --features fm-store/store-fusion = 521)
cargo test -p fm-engine --test mlx_live_extract --features mlx-live -- --include-ignored   # Single live test (needs fusion-mlx)
cargo clippy --workspace --all-targets -- -D warnings   # Lint (warnings are errors in CI)
cargo fmt --all --check                            # Format check
cargo bench -p fm-engine --bench retrieve_bench    # §13.2 perf baseline (store-stub 10k, no model)
cargo bench -p fm-engine --features mlx-live --bench retrieve_bench_live   # §13.2 live perf (real bge-m3 dim=1024, needs fusion-mlx; closes RC known-limit #1)
```

Toolchain: edition 2021, MSRV 1.87. System `rustc` (Homebrew, 1.96) compiles directly — no `rustup`, no `rust-toolchain.toml` (pinning 1.94 would block the Homebrew toolchain; Rule 7 deviation noted in README).

Coverage (`cargo-llvm-cov` + `llvm-tools`): run `cargo llvm-cov clean` first — stale profraw (including untriggered bench instrumentation) dilutes regions. Offline regions 90.82%; live regions 92.47% (PRD acceptance caliber = live, covers `engine.rs` summarize/consolidate saga + `engine_builder.rs` `!stub` branch). See `README.md` for exact commands + `fm-py` exclusion rationale.

## Rust Conventions (inherited from monorepo Rust projects)

- Indentation: **4 spaces** (multiples of 4)
- No docstrings on functions — `//!` module docs and code comments explain intent
- Logging: `tracing` crate (`tracing::info!`, `tracing::error!`) for all runtime logging
- Error handling: `anyhow` for application crates, `thiserror` for library crate error types
- Serialization: `serde` + `serde_json` throughout
- Async: `tokio` runtime
- Fail visibly — no silent error swallowing

## Fusion-MLX Integration

- LLM inference goes through `fusion-mlx` at `localhost:11434` (OpenAI-compatible API). Gateway variant at `127.0.0.1:11432`.
- Lifecycle: `~/claude-home/fusion-mlx/start.sh start|stop|status|log|doctor`
- Model downloads use mirror: `https://hf-mirror.com`; cache at `~/.fusion-mlx/models`
- Tests requiring real model inference must actually load the model (no mocks for integration tests)

## Conventions

- This is a **Rust** sub-project, not a Python domain app — do not assume the shared `.venv` applies here.
- Hard constraint inherited from the ecosystem: **100% offline** — no cloud API calls, no external network requests. HTTP only to `127.0.0.1`.
- Respect the business boundary strictly: vector page storage goes in `fusion-store`, code AST goes in `fusion-rag`. `fusion-memory` consumes `fusion-store`'s vector index, does not reimplement it.
- When the monorepo-wide CLAUDE.md (`<repo-root>/CLAUDE.md`) is updated to list `fusion-memory`, keep this file consistent with it.
