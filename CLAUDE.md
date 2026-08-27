# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

`fusion-memory` is the **system-level long/short-term memory and cognitive graph hub** of the Fusion ecosystem ("一核九端"). It exists to close the cross-session state gap that plagues Agents: repeated questions, lost context, and context-window blowup. The end goal is an Agent that knows the user better the more it is used.

Authoritative spec: `architecture/fusion-memory-prd-0825.md` (repo root `architecture/`).

## Current Status

**Built — M0 through M6 landed.** Rust Cargo workspace, 11 crates (`fm-cli`/`fm-core`/`fm-embed`/`fm-engine`/`fm-graph`/`fm-persist`/`fm-py`/`fm-server`/`fm-similarity`/`fm-store`/`fm-cluster`). 301 offline tests green, regions 90.82% offline / 92.47% live. Milestone detail + PRD deviation records in `README.md`. Remaining: M5 partially done (PII redaction + perf baseline landed; store-fusion switch + guard DLP gate degraded — see README "M5 PRD 偏离记录"). M4 outward integration PRs: `fusion-cowork` #68 + `fusion-agent-studio` #247 merged; `fusion-code` #151 open.

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
cargo test --workspace                             # All offline tests (301 cases, excludes fm-py cdylib)
cargo test -p fm-engine --test mlx_live_extract --features mlx-live -- --include-ignored   # Single live test (needs fusion-mlx)
cargo clippy --workspace --all-targets -- -D warnings   # Lint (warnings are errors in CI)
cargo fmt --all --check                            # Format check
cargo bench -p fm-engine --bench retrieve_bench    # §13.2 perf baseline (store-stub 10k, no model)
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
- When the monorepo-wide CLAUDE.md (`/Users/dahai/fusion/CLAUDE.md`) is updated to list `fusion-memory`, keep this file consistent with it.
