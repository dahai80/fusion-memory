# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

`fusion-memory` is the **system-level long/short-term memory and cognitive graph hub** of the Fusion ecosystem ("一核九端"). It exists to close the cross-session state gap that plagues Agents: repeated questions, lost context, and context-window blowup. The end goal is an Agent that knows the user better the more it is used.

Authoritative spec: `architecture/fusion-memory-prd-0825.md` (repo root `architecture/`).

## Current Status

**Greenfield — not yet scaffolded.** No `Cargo.toml`, `src/`, or build tooling exists yet. Work is PRD-driven: implement against the data structures and trait API defined in the PRD, following the monorepo Rust conventions below.

## Architecture (from PRD)

### Tech Selection

| Module | Choice | Rationale |
|--------|--------|-----------|
| Core language | Rust | High-performance memory control, no GC pauses during retrieval |
| Graph storage | SQLite + Kuzu DB (embedded graph) | Local lightweight embedded graph DB, no separate server process |
| Vector retrieval | `fusion-store` (HNSW) | Reuse the shared zero-copy vector index |
| Memory association | Apple Silicon NEON SIMD | Microsecond-scale cosine similarity and relevance decay on CPU / unified memory |

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

Not yet scaffolded. When implemented, follow the monorepo Rust pattern (see `fusion-cli` / `fusion-design`):

```bash
cargo check --workspace        # Compile check
cargo test --workspace         # Run all tests
cargo build --workspace        # Full build
cargo test -p <crate>          # Single crate
cargo fmt --check              # Format check
cargo clippy -- -D warnings    # Lint (warnings are errors in CI)
```

Rust toolchain follows the monorepo standard: channel `1.94` (see `fusion-design/rust-toolchain.toml`). Add a `rust-toolchain.toml` when scaffolding.

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
