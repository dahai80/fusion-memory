---
doc_id: fusion-memory/changelog
product: fusion-memory
class: core
entry: service
doc_type: changelog
title: { zh: "更新日志", en: "Changelog" }
audience: engineer
status: draft
version: 1.6.0
updated: 2026-11-12
owner: "@dahai"
source_of_truth:
  - CHANGELOG.md
---

# Changelog

All notable changes to fusion-memory. Format: Keep a Changelog. SemVer 2.0.0.
Internal path-dep private ecosystem (not on crates.io); versions tag + GitHub release.

## [Unreleased]

## [1.9.2] — 2026-11-14

PATCH release — embedding API key drift fix (issue #59). No wire-contract change; one new env (`FUSION_MEMORY_MLX_SETTINGS_JSON`).

### Fixed
- **Embedding API key drift (issue #59)**: the manually managed `[embed] mlx_api_key_file` silently goes stale when fusion-mlx rotates its key (authoritative source: `~/.fusion-mlx/settings.json` `auth.api_key`), surfacing only as a vague `vector store error: mlx api error: status 401` on retrieve. Three-part fix:
  - **Direct source**: when every explicit key source is unset (env `FUSION_MEMORY_MLX_API_KEY` / `FUSION_MEMORY_MLX_API_KEY_FILE`, TOML `[embed] mlx_api_key`/`mlx_api_key_file`), the server falls back to reading `auth.api_key` from the fusion-mlx settings dir — one source of truth, zero drift. Path overridable via `FUSION_MEMORY_MLX_SETTINGS_JSON` (default `$HOME/.fusion-mlx/settings.json`); read failure skips the fallback silently (startup never blocked; explicit config always wins).
  - **Fail visible**: an embedding 401 now returns an actionable error — "embedding key rejected by fusion-mlx — check [embed] mlx_api_key/mlx_api_key_file vs fusion-mlx settings.json auth.api_key (or unset them to use the settings.json fallback)" — instead of a bare store-layer error.
  - **Sync helper**: `fm doctor` and `./start.sh doctor` cross-check the key file (`$FM_HOME/mlx_api_key`) against fusion-mlx's `auth.api_key` and warn (start.sh: fail; fm-cli: DRIFT line) on mismatch, covering match / drift / absent / no-settings states.

### Verified (live)
- Server started with no `[embed]` key configured: commit + retrieve succeed against real fusion-mlx bge-m3 via the settings.json fallback; wrong explicit key produces the new 401 hint verbatim; `fm doctor` DRIFT/absent states and `./start.sh doctor` drift/healthy states exercised. 555 offline tests green (+3 fallback unit tests), clippy/fmt/live-compile/docs-validate clean.

## [1.9.1] — 2026-11-13

PATCH release — new client adapter (no Rust code/API change, no schema change), plus a docs-only subset-qualifier fix (#55).

### Added
- **fusion-harness MemoryBridge adapters (issue #42)**: `clients/python/harness_bridge.py` — `HarnessMemorySink` (implements the harness `MemorySink` protocol structurally: `store(key, payload, metadata=None)` persists a compaction summary as one Episodic memory via the `commit` RPC; session id falls back to the `scope:<session_id>` key suffix) and `HarnessMemorySource` (implements `MemorySource`: `retrieve(query, limit=5)` returns `[{payload, interaction_id, score}]` shaped for `MemoryBridge.inject_context`, bounded `top_k=limit`, network outage degrades to `[]` — recall is an upgrade, never a dependency). Sync `httpx` adapters (the harness protocols are sync), vendored-consumer model identical to `fusion_memory_client.py`, no `fusion_harness` import required. Tests (`clients/python/test_harness_bridge.py`, 7 cases): offline protocol conformance against the real fusion-harness tree (`runtime_checkable` Protocol isinstance), missing-API-key fail-closed, and live e2e against a real fm-server + fusion-mlx bge-m3 — sink store → source recall → `MemoryBridge.persist_compaction`/`recall`/`inject_context` roundtrip → bounded limit → outage no-op.

### Fixed
- **LongMemEval subset qualifier (PR #55)**: release banners in README.md / README_CN.md now carry "subset"/"子集" on the LongMemEval CI-gate mention — external-facing benchmark claims must always carry the subset qualifier (defense line 6 counterpart); every other outward mention was already qualified.

## [1.9.0] — 2026-11-12

Scale-and-finish (evolution plan 2026-11 phase 4, issues #51/#52/#53). MINOR release — additive only.

### Added
- **Cluster auto-failover activation (M9, #51)**: `spawn_cluster_with` DI variant (per-node `ElectionConfig` injection — NODE_ID is process-global env, cannot express multiple self_ids in one process; env-reading `spawn_cluster` unchanged). Split-vote livelock fix: campaign stagger by `self_id` (deterministic, no rand dep) + campaign skip while a recently-granted vote to another candidate is inside its lease + sitting-leader vote-deny (prevents double promotion via higher-term vote churn). 3-node live e2e (`crates/fm-server/tests/cluster_failover_3node.rs`): leader serves WOP sync (commit replicates to both followers over real TCP) → live leader killed → followers detect LeaderDown → quorum (2/3) promotion → role=Leader + epoch++ → restart simulation: promoted node serves on the new epoch and a fresh follower expecting epoch 1 receives the post-failover commit. Known limitation (documented): replayed rows are applied directly without a wop append, so a promoted follower-leader's wop log holds post-promotion writes only — a fresh catch-up node receives post-failover data, not pre-failover history (snapshot/compaction is future work).
- **Governance panel (issue #53)**: `config` RPC (UDS + HTTP `POST /v1/memory/config`) — read-only snapshot of the tuned scoring constants (`fm_engine::scoring::governance_snapshot()`: fusion weights 0.4/0.3/0.2/0.1, validity expired factor, θ_drop/θ_promote, episodic recall promotion, graph hop limit, three-way ablation arm). Web console: Governance tab rendering the snapshot; Audit tab gains an event filter (trust/injection-blocked, trust/injection-skipped, merge, cluster) — the poisoning-audit view on top of the v1.7.0 trust events. Provenance query (v1.7.0) cross-linked.

### Deprecated
- **fm-py (M10, #52 decision record)**: repo-wide scan found zero code consumers — every project (fusion-trainer included) accesses fusion-memory via the fm-server service surface (HTTP `:11435 /v1/memory/*` or UDS JSON-RPC). Per the plan rule, the binding is deprecated: stays compiling under the API freeze (removal would be breaking), no new feature investment; migration path documented in `fm-py` crate docs (run fm-server, use HTTP/UDS — multi-process safe, no exclusive data-dir lock).

## [1.8.0] — 2026-11-12

Skills and export (evolution plan 2026-11 phase 3, issues #48/#49). MINOR release — additive only: schema v9 (`memory_revision` table, `CREATE IF NOT EXISTS` idempotent — no ALTER), serde-default fields, new RPC methods on the frozen wire contract.

### Added
- **SKILL.md export (M7, #48)**: Procedural memories render as skills-contract documents — YAML frontmatter (`name` from first attached entity, fallback to memory id; `description` = first content line, ≤160 chars; `version: 1`; `source: fusion-memory`; `license: Apache-2.0`) + redacted body + `## Provenance` footer (`episode:`/`confidence:` lines) so consumers can judge trust. Deterministic template, no model call (Rule 5). `export_skills` RPC (UDS + HTTP `POST /v1/memory/export_skills`, limit 1..=50 default 20, namespace scoping, ACL owner filter).
- **Standard export (M8, #49)**: `export` RPC (UDS + HTTP `POST /v1/memory/export`) with `format: jsonl|markdown`, limit 1..=1000 default 500, optional namespace/namespace_key/memory_type scoping, tombstones excluded. JSONL = one serde round-trippable item per line (vector_ref blanked — local store handle meaningless off-node). Markdown = wikilink graph projection (`[[memory-id]]` + `[[entity-name]]`, Basic Memory / memU file-style boundary) with Sources/Confidence/Validity lines.
- **Revision history (M8, #49)**: schema v9 `memory_revision` (rev PK AUTOINCREMENT, memory_id, content, reason, at) — `put_memory`/`put_memory_with_wop` snapshot the PRIOR content via `maybe_record_revision` when it actually changes (idempotent replay safe; the old value is stored encrypted, decrypted on read — no extra plaintext at rest). `list_revisions` trait method (additive, default Unsupported) + `revisions` RPC (UDS + HTTP `POST /v1/memory/revisions`): subject invisible → 404; ACL: someone else's history → 404 (no leak). Merge write-back records revisions through the same put path.
- **Export redact gate (defense line 2)**: every export path (JSONL/Markdown/SKILL.md) applies `redact_text()` unconditionally inside the renderers — `FUSION_MEMORY_REDACT_PII=0` only covers the commit/import ingestion path, never egress. Acceptance tests assert no raw PII (phone/email) in any exported body.

### Changed
- fm-core: `MemoryRevision` DTO; `FusionMemoryEngine::list_revisions` additive trait method (default Unsupported).
- fm-persist: schema v9 (+ `idx_memory_revision_mid`); `list_revisions` store method.
- fm-engine: `export` module (`export_jsonl`/`export_markdown`/`render_skill`/`render_skills`).

## [1.7.0] — 2026-11-12

Time and trust (evolution plan 2026-11 phase 2, issues #44/#45/#46). MINOR release — additive only: schema v6 (validity columns) / v7 (confidence + sources_json) / v8 (owner) incremental guarded migrations, serde-default data fields, boundary-level ACL. Frozen wire contract untouched.

### Added
- **Validity windows (M3, #44)**: `valid_from`/`valid_until` (epoch ms) on MemoryItem (serde default, old rows = no window), set via free-form `interaction.metadata` keys — zero wire-contract change. `validity_of()`: no window → 1.0, not-yet-valid → 0.0, expired → `VALIDITY_EXPIRED_FACTOR` 0.1 (retrieval-downweighted, never deleted; tombstone stays explicit-delete-only). Score = `fuse_score4 x validity` — multiplicative stacking with the forgetting curve (defense line 3). Ablation `validity_window_ablation_never_below`: setup arm (no windows) expired freshness-heavy item wins all 40 topics; with windows the relevant item wins all 40; window-free items bit-identical across arms (never-below gate).
- **Memory trust chain (M6, #45)**: `fm-engine::trust` — `InjectionScanner` trait + `LocalScanner` (rule subset INJ-001..007 ported byte-identical from fusion-security, case-insensitive, always installed = 100% offline fail-safe floor) + `HttpScanner` (opt-in `FUSION_MEMORY_INJECTION_URL` to fusion-security `POST /api/v1/agent/injection-scan`, loopback-only enforced at builder — non-loopback refuses to start; `FUSION_MEMORY_INJECTION_TOKEN` bearer) + `scan_with_fallback` (rule_id dedup; scanner outage degrades to local rules — detection never drops to zero). Injection gates on every LLM-ingestion path: commit with flagged text skips entity extraction (memory still lands, `trust/injection-blocked` audit); consolidate distillation partitions poisoned inputs out (`injection-skipped` audit, `SUMMARIZE_MIN_EPISODIC` applies to the clean set); merge skips flagged pairs. Consolidation products: `confidence` (deterministic corroboration ladder 0.5 / 0.75 / 0.95 — no model call) + `sources` (distinct episode interaction ids; merges accumulate source ids); reverse citations via list scan. `provenance` RPC (UDS + HTTP `POST /v1/memory/provenance`) returns `{id, sources, confidence, cited_by}`; web console Provenance tab = poisoning governance surface. Upstream contract coordination: fusion-security#85.
- **Per-memory ACL (M5, #46)**: identity source = the existing fusion-identity JWT boundary — `verify` additionally extracts `sub`/`role` (serde default: older identity responses → empty = fail-closed). `FUSION_MEMORY_ACL=true` (+ TOML mirror) requires `multi_tenant=true`; `validate()` refuses otherwise (ACL without an identity source = fail-visible startup error; no bypass identity channel — tenant still only trusts the gateway stamp, defense line 5). Commit stamps `owner` (schema v8) authoritatively from the server-verified JWT sub (client-supplied owner overwritten). Enforcement at the fm-server boundary (UDS stays trusted-local; engine identity-agnostic): `get`/`delete`/`provenance` deny with NotFound (no existence leak), `retrieve`/`memory.retrieve_context` drop invisible blocks, `list` filters rows; `role=admin` bypasses; owner-less (legacy/shared) rows visible to everyone. Multi-user contract tests (`crates/fm-server/tests/acl.rs`): owner isolation, cross-user 404, admin bypass, delete denial, multi-user retrieve isolation, shared-memory visibility, ACL-off backward compat. E2E trust-chain tests (`crates/fm-server/tests/trust_chain.rs`): in-process mock fusion-security contract, scanner-outage degradation, commit-gate audit.
- **Web console**: Provenance tab (trace id -> confidence / sources / reverse citations badges).

### Changed
- fm-core: MemoryItem + `valid_from`/`valid_until`/`confidence`/`sources`/`owner` (all serde-default); Interaction + `owner`; ContextBlock + `owner`; `ProvenanceReport` DTO; `FusionMemoryEngine::provenance` additive trait method (default Unsupported).
- fm-server: `ServerConfig` + `acl`; identity `VerifyResult` now returns `CallerCtx {tid, sub, role}` (crate-internal); `dispatch` + optional `AclUser` param (UDS passes None).

## [1.6.0] — 2026-11-12

Retrieval and consolidation (evolution plan 2026-11 phase 1, issues #39/#40/#41). MINOR release — additive only: schema v5 (FTS5 virtual table), new optional scheduler task (default off), new test harnesses. Frozen wire contract untouched.

### Added
- **Four-way hybrid retrieval (M1, #39)**: FTS5 trigram keyword index (`memory_fts`, schema v5 with backfill migration) fused with cosine / graph affinity / freshness — weights 0.4 / 0.3 / 0.2 / 0.1 (`fuse_score4`). Lexical candidates oversampled 4x and reranked; FTS term extraction dual-track (raw punctuated tokens + alphanumeric runs, ≥3 chars). Cipher deployments keep no plaintext token index (FTS sync skipped on write, `fts_search` fail-safe empty — B-1 field encryption stays airtight). Ablation: full-scale recall@5 cosine 0.2496 → three-way 0.5443 → four-way 0.6779, lexical recovery 40/40 vs 0/40 (`docs/benchmarks/retrieval-quality-m1-fourway-2026-11-12.md`); CI gates `ablation_four_way_never_below` + `lexical_hits_recovered_by_four_way`.
- **Dreaming scheduler (M2, #40)**: idle-triggered background consolidation in fm-server — tokio timer + in-flight gate (`MemoryEngine::inflight_count`, RAII guard on retrieve/commit) + pending-entity threshold (`pending_count`); trigger calls the same `consolidate_memories()` entry as the manual RPC (consolidate_lock serializes both; saga kernel untouched). Boundary events (trigger/complete/fail) land in audit_log via `dream_audit`. Default off: `FUSION_MEMORY_DREAM_INTERVAL_SECS` (0 = disabled) / `FUSION_MEMORY_DREAM_PENDING_THRESHOLD`.
- **LongMemEval-subset CI gate (M4, #41)**: LongMemEval-shaped synthetic corpus (20 needle instances, near-miss adversarial sessions, filler haystack) through the real engine path; dual embedder mode (StubEmbedder CI floor / real bge-m3 live, feature `mlx-live`); committed baseline JSONs + never-below asserts + `FM_LME_UPDATE=1` regeneration. Stub and live both recall@5 1.0 / MRR 1.0 (`docs/benchmarks/longmemeval-subset-2026-11-12.md`).

### Changed
- `SCHEMA_VERSION` 4 → 5 (memory_fts virtual table + plaintext-row backfill migration).
- FTS term/punctuation fix recorded in bring-up: trailing punctuation must not glue onto rare terms (see benchmark doc notes).

## [1.5.1] — 2026-11-07

Docs-system adoption (fusion docs-system v1.1 contract, core x service). Docs-only patch: no Rust code change, no API change, all 12 crate versions bumped for release bookkeeping only.

### Added
- **docs/ page set**: `product-page`, `get-started`, `architecture`, `api-reference` (now documents `GET /healthz`, `/metrics`, `GET /v1/memory/version`), `config-reference`, `troubleshooting` — every page carries the docs-system frontmatter contract (`doc_id`/`doc_type`/`status`/`source_of_truth`).
- **`ci/validate_docs.py`** (+ `.github/workflows/docs.yml`): docs-contract linter — frontmatter completeness, doc-type budgets, source-of-truth consistency; 14 docs scanned, 0 errors.
- **AGENTS.md**: agent-facing working guide aligned with the same contract.

### Changed
- `CLAUDE.md` / `README.md` / `README_CN.md` / `clients/README.md` / `deploy/README.md` / `CHANGELOG.md` gain the docs-system frontmatter header; content semantics unchanged.

## [1.5.0] — 2026-11-05

Web surface (issue #33): browse/manage REST APIs + embedded web console. MINOR release — additive RPC methods and HTTP routes only, frozen wire contract untouched (existing method set and `v1.<method>` prefix unchanged).

### Added
- **Browse/manage RPC methods** (UDS + HTTP, same dispatch): `list` (paged memory browse, filters `memory_type`/`namespace`/`namespace_key`/`include_tombstoned`, `offset`/`limit` hard-capped 1..=200), `audit_log` (latest N entries, limit 1..=500), `merges` (full merge log), `unmerge` (`confirm=true` required, B5), `reconcile` (`confirm=true` required, B5). Backed by five new additive `FusionMemoryEngine` trait methods (`list_memories`/`list_audit_log`/`list_merges`/`unmerge`/`reconcile`) with default `Unsupported` impls — same extension pattern as v1.2.0 tenant methods; production `MemoryEngine` overrides all five. Browse DTOs (`ListQuery`/`AuditEntry`/`MergeEntry`) live in fm-core (dependency direction respected: fm-core cannot depend on fm-persist row types). Tenant scoping: `list` filters in-memory on `list_tombstoned` (no SQL tenant filter upstream) to prevent cross-tenant leaks.
- **Embedded web console** (`GET /app`, `/app/*path`): React 18 + Vite + Tailwind v4 + lucide-react thin client over the REST surface — Entries (paged browse + filters), Audit log, Merges (per-row unmerge + reconcile), Import (agent-studio history), Bearer key login (localStorage). UI source in `crates/fm-server/web/`, built `dist/` embedded at compile time via `include_dir` (no runtime file dependency, CI needs no bun); static shell is public, all data still flows through Bearer-guarded `/v1/*`.
- **HTTP import route** (`POST /v1/memory/import`, HTTP-only): HTTP version of `fm import` — body IS the params object (`{"source": "..."}`, defaults to `~/.fusion-agent-studio/memory.db`), returns a bare import report. The route reuses the server's concrete `MemoryEngine` (shared SQLite WAL + store handle) — building a second engine hit the sled exclusive lock; embedder matches the server's (stub server = stub import). Core implementation moved fm-cli → fm-engine::import_studio (CLI and server share one implementation; CLI keeps home-resolution semantics).

### Deviation (issue #33)
- The issue suggested "chunked import with progress streaming"; delivered synchronous import returning the final report (imported/skipped_archive/skipped_empty/failed). Chunked progress streaming deferred — recorded here per Rule 12.

### Acceptance
- `cargo fmt --all --check` clean; `cargo clippy --workspace --all-targets -- -D warnings` clean; `cargo test --workspace` 496 passed (32 suites), `--features fm-store/store-fusion` 502 passed; live-compile ×3 (`fm-embed`/`fm-engine`/`fm-server` with `RUSTFLAGS="-D warnings" --features mlx-live`) clean.
- Real-binary smoke (stub, port 11498): `/app` shell 200, `/app/assets/*.js` 200, `list` JSON-RPC roundtrip, `audit_log` roundtrip, `import` of a synthetic 1-row agent-studio db → `{"imported":1,...}`, imported entry visible via `list`.

## [1.4.5] — 2026-10-30

start.sh registry self-check (issue #31, config governance Phase 3.3). Shell-only patch: no Rust code change, no API change.

### Added
- **`start.sh` port-registry self-check**: after the port-in-use check, `start.sh start` verifies the configured `FUSION_MEMORY_HTTP_PORT` is registered for `fusion-memory` in `port-registry.yaml` (`FUSION_REGISTRY`, default `~/fusion/architecture/port-registry.yaml`) and refuses to start (exit 1) otherwise — the CI `check-ports.py` failure moves forward to first boot. Missing registry file = warn + continue (standalone deploy); `FUSION_SKIP_REGISTRY_CHECK=1` bypasses with a warning. Acceptance verified against temp registry copies: unregistered port refuses, registered port starts, bypass and missing-file paths warn + start.

## [1.4.4] — 2026-10-28

Env contract documentation + check-env false-positive fix (issue #29, config governance Phase 2.2). Docs-only patch: no code change, no API change.

### Added
- **"Environment variables" section** (README.md / README_CN.md): consolidated `FUSION_MEMORY_*` inventory — server (fm-server/src/config.rs), embedding (fm-embed + `[embed]` TOML mirror), engine/persistence (PII redaction, static encryption, wop retention), cluster (fm-cluster election + sync) — each with default, purpose, and source file. Resolution priority chain documented: env > TOML (`FM_CONFIG` / `<data_dir>/fusion-memory.toml`) > secret file > default.

### Fixed
- **check-env `[legacy]` false positive** (workspace `scripts/check-env.py`, outside any git repo): rule 2c matched legacy names by line-level substring, so compliant `FUSION_MEMORY_MLX_API_KEY` (v1.4.1 API contract) was flagged for containing the substring `MLX_API_KEY` (`start.sh` lines 17/192 — both reference the compliant name and cannot be renamed). Rule 2c now extracts `FUSION_*` tokens and matches legacy names by exact prefix (alias-chain variants like `FUSION_MLX_API_KEY_FILE` still caught; the trailing-underscore `FUSION_FUSION_` bug pattern keeps substring matching, same source as rule 2a). Verified with a matcher unit matrix (11 cases) and a full workspace scan: this repo drops to **0 findings**; genuine legacy references in other repos still warn (workspace warn total 136 → 132, the 4 dropped findings diff-verified as false positives).

### Acceptance
- `python scripts/check-env.py` (workspace root): 0 non-baseline findings for this repo — issue #29's required-rename list was empty; every name already conforms to conventions §1.

## [1.4.3] — 2026-10-26

CI-red fix: cross-repo path dependencies replaced with pinned git tag dependencies (issue #27). The v1.4.2 commit landed red on CI — a runner checks out fusion-memory alone, so the sibling monorepo directories don't exist and the whole workspace manifest fails to load before any build step.

### Fixed
- **`fg-redact` (fm-engine)**: `path = "../../../fusion-guard/crates/fg-redact"` → git tag dep `dahai80/fusion-guard` `v0.2.0-rc.7` (same commit as the local working tree — zero drift).
- **`fs-core` (fm-store, optional `store-fusion`)**: `path = "../../../fusion-store/crates/fs-core"` → git tag dep `dahai80/fusion-store` `v0.2.0-rc.3` (same commit as the local working tree — zero drift). Optional deps participate in manifest resolution, so this broke the default build too, not just the feature build.
- Verified with a standalone `git clone` (no sibling repos): `cargo test --workspace` green, all 32 suites including the retrieval-quality gate (which had never executed in CI before).
- "100% offline" remains a **runtime** constraint (HTTP only to 127.0.0.1); build-time cargo fetches each git dep once and caches it in `~/.cargo/git`. Local upstream co-development: override with a `[patch."https://github.com/dahai80/fusion-guard"]` section.

## [1.4.2] — 2026-10-25

Retrieval quality evaluation + fusion-score retune (issue #25). The forgetting-curve decay, graph affinity, and fusion scoring were real algorithms with zero retrieval-quality evidence; this lands the ablation benchmark and acts on its first finding.

### Added
- **Retrieval quality benchmark** (`crates/fm-engine/tests/retrieval_quality.rs`): deterministic synthetic corpus (seeded xorshift64*, fixed `now`, in-memory SQLite graph), ground-truth queries, and four+one ablation arms — plain cosine / cosine+decay / cosine+graph / full fusion / pre-retune raw-W (kept as the decision record). Metrics: recall@5 / nDCG@10 / MRR. CI gate asserts full fusion never below plain cosine, no single component drags, fresh-over-stale temporal wins ≥90% (fusion 40/40 vs plain 18/40), and bitwise seed determinism. Full scale via `FM_RQ_SCALE=full` (400 topics / 5200 items / 400 queries, ~10s).
- **Results + decision** (`docs/benchmarks/retrieval-quality-2026-10-25.md`): full fusion beats plain cosine by +0.40 recall@5 / +0.32 nDCG@10 / +0.25 MRR at full scale.

### Changed
- **Fusion-score decay term retuned**: `score_candidate` now uses `freshness_of()` (normalized freshness: `exp(-t/τ(type))·reinforce`, w0 stripped, capped at 1.0) instead of raw `W(t)`. First benchmark run proved the raw-`W(t)` fusion was **worse than plain cosine** (recall@5 0.4250 → 0.2285 full scale): `W(t)` carries the type-dependent w0 (0.6/0.8/1.0), so the additive β·W(t) term promoted Procedural/Semantic items regardless of relevance. α/β/γ weights unchanged; raw `W(t)` still drives the θ_drop/θ_promote scheduling thresholds — only retrieval ranking changed. `crates/fm-engine/src/scoring.rs`.

### Tests
- Default features: 474 → 478 (+4 retrieval-quality tests). Gates: fmt / clippy -D warnings / test all green.

## [1.4.1] — 2026-10-24

MLX embedding API key TOML mirror (issue #23). Previously the fusion-mlx embedding key was only readable from env `FUSION_MEMORY_MLX_API_KEY` — a default `start.sh` service start against key-enforced fusion-mlx silently produced 401s on `/v1/embeddings`. Fully backward compatible: no `[embed]` section and no env = old behavior.

### Added
- **`[embed]` TOML section** (`fm-server/src/config.rs`): `mlx_url` (default `http://127.0.0.1:11434/v1`), `embed_model` (default `BAAI/bge-m3`), `mlx_api_key`, `mlx_api_key_file` (0600 secret file, recommended over env). Priority unchanged: env > TOML field > secret file > default. Env secret path `FUSION_MEMORY_MLX_API_KEY_FILE` also supported.
- **`EmbedConfig::overlay_env()`** (`fm-embed`): applies env overrides on top of an existing (TOML/default) base; `from_env()` now delegates to it. `build_server_engine` resolves the config once and shares it between the embedder and the entity extractor (extractor no longer reads env directly, so it stops 401-ing independently of the embedder).
- `start.sh` header documents the `[embed]` section.

### Tests
- Default features: 468 → 474 (+6: fm-embed overlay base/env precedence, fm-server TOML override / env-over-TOML / secret file / env secret path / absent-section defaults).

## [1.4.0] — 2026-10-21

Scoped memory namespaces (issue #21, consumer fusion-agent-studio#434). Adds a second, tenant-orthogonal visibility dimension `namespace` (`user` / `project` / `local` / empty) to commit, retrieve, and consolidation. Fully additive: empty namespace = old behavior (global), and all new fields are `#[serde(default)]` so old JSON / WOP payloads replay unchanged.

### Added
- **Namespace data fields** (`fm-core`): `MemoryItem.namespace` / `namespace_key`, `Interaction.namespace` / `namespace_key`, `RetrieveQuery.namespace` / `namespace_key` (all `#[serde(default)]`). `MemoryItem::with_namespace()` builder keeps the 8-arg `new_turn_skeleton` signature stable. New `fm_core::namespace` module: constants, `is_valid_namespace`, `NamespaceFilter` with in-memory `visible()` stacking rule.
- **Visibility stacking**: `project` sees its own key ∪ `user`; `local` sees only its own key (no user stacking); `user` sees user items only; empty = no filter. Unknown namespace values are rejected fail-closed (`PersistError::Namespace` → `MemoryError::Unsupported`, RPC `-32602`) instead of silently reading everything.
- **Schema v4** (`fm-persist`): `namespace TEXT NOT NULL DEFAULT ''` + `namespace_key TEXT NOT NULL DEFAULT ''` columns and `idx_memory_namespace(namespace, namespace_key, tombstone)`. Idempotent `ALTER TABLE` via `pragma_table_info`. `SCHEMA_VERSION` 3 → 4. Migration order fixed: main table first, then `ALTER`, then indexes (index DDL references the new columns).
- **Scoped persist queries** (`fm-persist/src/store.rs`): `get_by_vector_refs_tenant_scoped`, `list_changed_since_scoped`, `list_all_by_tenant_scoped` plus `ns_predicate()` SQL builder (project stacking / local isolation / user key refinement). Original tenant-only methods kept intact and now delegate.
- **Engine scoping** (`fm-engine`): commit stamps `interaction.namespace` onto every turn item; `retrieve_context` passes the filter into the KNN hit lookup and over-fetches `top_k × 4` when a namespace filter is active (store trait has no metadata filter, so post-KNN filtering would otherwise under-fill); `consolidate_memories_scoped()` limits recycle/promote/merge/summarize/reconcile to the visible domain and does **not** advance the global consolidation watermark (`consolidation_log` MAX(started_at)) — scoped runs are backfills and must not swallow other namespaces' pending changes. Summary items inherit the source session namespace.
- **RPC surface** (`fm-server/src/jsonrpc.rs`): `namespace` / `namespace_key` params on `commit`, `retrieve`, `memory.retrieve_context`, and `consolidate` (params optional; `{}` / null still works). Explicit commit params override interaction values (same authoritative-parameter pattern as tenant). Named `namespace` to avoid colliding with the existing `delete_scope` / `count` `scope` (session_id) semantics.
- **Clients**: Python and TypeScript reference clients gained optional namespace arguments; `fm-py` `retrieve_context` gained `namespace` / `namespace_key` kwargs with validation.
- **Tests** (+10): namespace filter unit tests (stacking, fail-closed, validation), persist scoped-query tests + v3→v4 migration, engine stacking/watermark tests, jsonrpc passthrough/validation/override, and an HTTP end-to-end scoping test on the real server chain.

### Changed
- Version bump all 12 crates `1.3.0` → `1.4.0` (additive API; SemVer MINOR, not PATCH).
- `migrate()` reordered: `CREATE TABLE` → `ALTER TABLE` → `ALL_DDL` (previously index DDL ran before column migration, which fails when the index references a column the old database lacks).

### Test counts
- Default features: 452 → 468 (+16 namespace tests).
- Gates: fmt / clippy -D warnings / check / test all green.

### Upstream status
- Consumer-side implementation (subagent `memory:` field, project-root key derivation, per-subagent toggle) lives in **fusion-agent-studio#434**, out of this repo's scope. Wire contract documented in `clients/README.md`.

## [1.3.0] — 2026-09-03

fusion-identity integration (issue #18). fm-server HTTP boundary enforces caller JWT verified by fusion-identity (`POST /api/v1/auth/verify`), tid↔tenant match, and rejects empty tenant in multi-tenant mode (multi-tenant PRD §4 red line 1 fail-closed). Single-tenant backward compatible (default `multi_tenant: false` = no identity verify).

### Added
- **Identity verifier module** (`fm-server/src/identity.rs`): `IdentityVerifier` trait (`verify` + `report_usage`) enables 100% offline testability (no real fusion-identity service in tests). `RealVerifier` (reqwest client to `127.0.0.1` only, 30s TTL result cache via `Arc<Mutex<HashMap>>`) — fail-closed on HTTP error. `NoopVerifier` + `noop_verifier()` for single-tenant backward compat. `enforce_multi_tenant()`: `multi_tenant=false` short-circuits; true + empty tenant → 401; true + missing/empty Bearer JWT → 401; verify → tid != tenant → 403; match → Ok(tid).
- **Multi-tenant config** (`fm-server/src/config.rs`): `multi_tenant: bool` (default false), `identity_url` (default `http://127.0.0.1:11470`), `identity_service_token` (+ `_file` secret read). Env: `FUSION_MEMORY_MULTI_TENANT` / `FUSION_MEMORY_IDENTITY_URL` / `FUSION_MEMORY_IDENTITY_SERVICE_TOKEN` / `FUSION_MEMORY_IDENTITY_SERVICE_TOKEN_FILE`. `validate()`: `multi_tenant=true` requires http identity_url AND non-empty service_token, else Err. 4 new config tests.
- **HTTP boundary enforcement** (`fm-server/src/http.rs`): in multi-tenant mode `check_bearer` (static api_key) is skipped — Bearer token is the JWT (verified by identity). `handle_rpc` + `get_memory` call `enforce_multi_tenant` after tenant extraction. `HttpState` gains `multi_tenant: bool` + `verifier: Arc<dyn IdentityVerifier>`. 7 new http tests: cross-tenant JWT denied (403), empty tenant rejected (401), matching tid passes (200), invalid/revoked JWT rejected (401), missing JWT rejected (401), cross-tenant get denied (403), single-tenant mode skips identity verify (200).
- **Identity e2e** (`fm-server/src/identity.rs` tests): in-process axum echo server stands in for fusion-identity (100% offline) — good-jwt→tenant-x pass, revoked JWT denied, cache hit, unreachable endpoint fail-closed.

### Changed
- Version bump all 12 crates `1.2.0` → `1.3.0` (additive config + boundary enforcement; single-tenant default behavior unchanged).
- `fm-core/src/memory.rs` `new_turn_skeleton`: `#[allow(clippy::too_many_arguments)]` (8 args after v1.2.0 tenant field).
- `fm-server/src/tenant.rs`: collapsed nested `if` (clippy collapsible_if), blank line before trailing doc paragraph (doc_lazy_continuation).

### Test counts
- Default features: 433 → 452 (+19 identity tests: 4 config + 7 http + 8 identity unit/e2e).
- `--features fm-store/store-fusion`: 439 → 458.
- Gates: fmt / clippy -D warnings / check / test all green.

### Upstream status
- **fusion-identity** (consumed, not modified): sole JWT issuer + tenant registry. fm-server calls `POST /api/v1/auth/verify` (service-token gated) + `POST /api/v1/tenants/{tenant_id}/usage` (best-effort). Three red lines honored: fail-closed, cross-tenant denied (tid must match tenant), data isolation layering.

## [1.2.0] — 2026-09-02

Multi-tenant isolation (issue #16). Backend half of fusion-gateway #150 Gap1c. Canonical pattern follows fusion-model-hub#53. Single-tenant backward compatible (tenant="" = default tenant).

### Added
- **Tenant field on data structures** (`fm-core`): `MemoryItem.tenant`, `Interaction.tenant` (`#[serde(default)]`), `RetrieveQuery.tenant` (`#[serde(default)]`). Tenant flows through data, NOT through core `FusionMemoryEngine` trait method signatures — preserves v1.0 API freeze.
- **Additive tenant-scoped trait methods** (`fm-core::FusionMemoryEngine`): `get_memory_tenant` / `delete_memory_tenant` / `delete_scope_tenant` / `count_tenant` — default-delegate to non-tenant variants. Backward compatible; impls override for real scoping.
- **Schema v3 migration** (`fm-persist`): `tenant TEXT NOT NULL DEFAULT ''` column + `idx_memory_tenant` index. Idempotent `ALTER TABLE` via `pragma_table_info` check. SCHEMA_VERSION 2 → 3.
- **Tenant-scoped store queries** (`fm-persist/src/store.rs`): `list_all_by_tenant`, `get_by_vector_refs_tenant`, `count_by_tenant`, `delete_by_session_tenant`, `count_by_session_tenant`, `list_by_session_tenant`. Original methods kept intact.
- **Engine tenant scoping** (`fm-engine`): `MemoryEngine.tenant` field (startup default, `""`), `with_tenant` builder. commit/summarize/retrieve/consolidate all scope by `self.tenant`. Trait overrides: cross-tenant get → None (no existence leak), cross-tenant delete → NotFound, missing-row delete → Ok (preserves soft-delete semantics).
- **Gateway-origin enforcement + tenant extraction** (`fm-server/src/tenant.rs`): `X-Fusion-Route: gateway-decision` header (config `gateway_origin_required: bool = false` default off); `X-Fusion-Tenant` authoritative tenant (priority: header > `default_tenant` config > ""). HTTP `handle_rpc` + `get_memory` call `check_gateway_origin` before dispatch. UDS (trusted-local) uses default tenant. Config env: `FUSION_MEMORY_GATEWAY_ORIGIN_REQUIRED`, `FUSION_MEMORY_DEFAULT_TENANT`.
- 4 new tests in `fm-server/src/http.rs`: gateway-origin required rejects missing route (403), accepts valid route, not-required passes, cross-tenant get isolation (X-Fusion-Tenant: other → result null).

### Changed
- Version bump all 12 crates `1.1.0` → `1.2.0` (additive API — new fields/methods have defaults; no breaking change to existing callers).

### Test counts
- Default features: 429 → 433 (+4 gateway-origin/tenant tests in fm-server).
- `--features fm-store/store-fusion`: 435 → 439.
- Gates: fmt / clippy -D warnings / check / test all green.

## [1.1.0] — 2026-08-29

Commercial GA. Closes the three RC known limitations from [1.1.0-rc.1] (store-stub naming, interim redact.rs, StubEmbedder perf baseline), all from user request "现在建 store-fusion adapter ，然后换上游fg-redact" + GA path "起 fusion-mlx 测真 perf".

### Added
- **store-fusion adapter** (`fm-store/src/fusion.rs`, feature `store-fusion`): real `FusionStoreEngine` impl wrapping upstream `fusion-store` fs-core `Engine` (HNSW + mmap KV). Closes RC known-limitation #2 (store-stub naming — store-fusion now a real alternative backend, not just "stub"). Distance semantics bridge: fs-core returns `distance = 1 - cos_sim`, adapter converts `similarity = 1.0 - distance` to match fm-store contract (same formula as local.rs). UFCS calls to fs-core trait (same trait name `FusionStoreEngine` in both crates). ZeroCopyBuffer mmap→owned bridge. 6 tests (kv roundtrip / vector insert+get+search / dim mismatch / search dim mismatch / delete→none / list_ids excludes deleted). **Additive, not exclusive**: coexists with local-store (local-store default + always-on; store-fusion optional, default off). Both compile together.
- **fg-redact credentials integration** (`fm-engine/src/redact.rs`): stage-1 credential redaction delegates to upstream `fg-redact::Redactor::redact_credentials()` (PR fusion-guard#11 / issue #10). fg-redact covers 10 credential classes (JWT/private_key/oauth_bearer/api_key/conn_string/password/secret_kv/env_kv/netrc/aws_secret) that fusion-memory previously lacked. Stage-2 PII stays fusion-memory-local (phone+86/0086/email/idcard/bankcard+Luhn/IPv4/passport/IPv6/intl) — fg-redact's PII behavior is worse (idcard eaten by credit_card, long non-Luhn digits eaten by id_number, +86 phone rejected by border validator), so PII not routed through fg-redact. Closes RC known-limitation #3 interim redact.rs (credentials now upstream; PII remains local by design, documented in redact.rs module doc). 4 new tests (jwt/password/credential+pii/idcard-stays-local-not-bankcard). Idempotent: credential placeholders `[REDACTED:jwt]` have no digits → PII regex no re-match.
- **live perf baseline (real bge-m3)** (`fm-engine/benches/retrieve_bench_live.rs`, feature `mlx-live`, `required-features`): end-to-end retrieve latency with real fusion-mlx bge-m3 (dim=1024) via `MlxEmbedder` HTTP `/v1/embeddings`. Closes RC known-limitation #1 (Perf baseline = StubEmbedder — real bge-m3 latency now measured). Three paths measured: cold (unique query, real mlx each), cached (LRU hit, skip mlx HTTP, index-layer only), concurrent x5 (real mlx). Baseline JSON: `benches/baseline-live-bgem3-2026-08-28.json`. Scale constrained by fusion-mlx rate-limit bug (upstream issue #692: `_serve_from_model_dir` path misses `configure_rate_limiter`, so module-level `RateLimiter(60, enabled=True)` stays on even with `--rate-limit 0`; #635/#637 fixed only the other two serve paths); small-scale real-latency reference, large-scale stress stays on `retrieve_bench.rs` (StubEmbedder, no model). #692 fixed → scale can be raised and bench re-run. Live data (Apple Silicon, release): cold p50=9.98ms / p99=10.73ms; cached p50=0.107ms / p99=0.126ms; concurrent x5 p50=44.6ms.

### Changed
- `fm-engine` re-adds `fg-redact` path dep (was removed in earlier revert).
- `fm-store` `store-fusion` feature is now additive (not exclusive with local-store). `default = ["local-store"]`, `store-fusion = ["fs-core"]` optional.

### Test counts
- Default features: 425 → 429 (+4 credential tests in fm-engine redact).
- `--features fm-store/store-fusion`: 435 (429 + 6 store-fusion tests).
- Gates: fmt / clippy -D warnings / check / test all green.

### Upstream status (this release closes two)
- **fusion-guard #10 / #11** — credentials-only redact API (`redact_credentials` + `redact_with_patterns` + `CREDENTIAL_PATTERNS` const). Issue #10 filed, PR #11 implements + 8 issue10_* tests. fusion-memory consumes `redact_credentials()`. **Resolved**.
- **fusion-store #3 / #4** — zero-copy backend. Adapter built (consuming fs-core via path dep). Upstream #3 (expose `get_vector`/`list_vector_ids`) + #4 closed by upstream. store-stub remains default production backend; store-fusion optional.

## [1.1.0-rc.1] — 2026-08-28

Release candidate for 1.1.0 commercial GA. Hard commercial blockers closed + real-tested (not paper). Soft caveats documented below — non-blocking for RC, resolve or accept before GA.

### Added
- B-2 auto-failover e2e integration test (`fm-server/tests/election_failover.rs`): drives production entry `spawn_cluster(role=Follower)` + real MemoryEngine + real in-process TCP, verifies full `follower_orbit` chain (leader down → LeaderDown → campaign → quorum → epoch++/role=Leader write → detect Leader). Closes prior gap: README claimed auto-failover but only unit tests + manual promote existed.

### Changed
- Version bump all 12 crates `1.0.1` → `1.1.0-rc.1` (pre-release).

### RC readiness state (verified this release)

| Area | Status | Evidence |
|------|--------|----------|
| B-2 auto-failover | real-tested | election_failover.rs e2e, 425 tests green |
| B-1 static encryption | real | AES-256-GCM in store.rs put/hydrate, `enc:v1:` prefix, fail-open mixed read |
| C-1 API 1.0 | real | 12 crates lockstep, semver commitment |
| C-2 fuzz + perf | real | fm-fuzz libfuzzer target + retrieve_bench 100k concurrency gradient |
| Gates | green | fmt / clippy -D warnings / check / 425 tests |
| Deploy artifacts | real | systemd + launchd + Dockerfile distroless, /metrics, backup |
| Coverage | pass | live regions 92.47% |

### Known limitations (RC, documented — non-blocking)

1. **Perf baseline = StubEmbedder**. retrieve_bench 100k uses stub (no mlx HTTP latency). Real bge-m3 latency not measured. Index-layer perf only. **Resolved in [Unreleased]** — `retrieve_bench_live.rs` now measures real bge-m3 end-to-end latency (cold/cached/concurrent). Large-scale stress (100k) still uses StubEmbedder (no model) by design; live bench is small-scale real-latency reference (fusion-mlx rate-limit constraint).
2. **store-stub naming**. hnsw_rs + sled backend, long-term production per README, but named "stub". Documented as production backend, not temporary. **store-fusion adapter (Unreleased) now provides the real fusion-store-backed alternative — see [Unreleased].**
3. **Upstream items (tracked, not fusion-memory code)**:
   - fusion-store #3/#4 — zero-copy backend. Upstream #3/#4 now closed; store-fusion adapter (Unreleased) consumes fs-core via path dep. fusion-memory uses store-stub as default.
   - fusion-guard #2/#13 — formal DLP PII gate. #2 closed (PII pattern classes upstream). #13 open (fg-redact PII behavioral defects — idcard eaten by credit_card, +86 phone rejected; PII stays fusion-memory-local by design, see redact.rs doc, not blocked on #13). Credentials part resolved in [Unreleased] (fusion-guard #10 closed / PR #11 merged, `redact_credentials` API consumed). Full UDS `guard.redact` DLP gate still future.
   - fusion-mlx #692 — rate-limit bug on `_serve_from_model_dir` path (blocks large-scale live bench; #635/#637 fixed other two paths). Filed this release. Small-scale live bench works around it.
   - GitHub Actions CI billing-blocked — ops, not code
4. **P2-5 Persist split / P2-6 dep migration** — deferred, non-blocking.

### Deploy prerequisites (ops checklist, full in deploy/README.md)
- FDE on (macOS FileVault / Linux LUKS) — primary at-rest encryption
- `FUSION_MEMORY_API_KEY` set when HTTP port open
- `FUSION_MEMORY_CLUSTER_TOKEN` + NODES + NODE_ID (cluster mode)
- `FUSION_MEMORY_ENC_KEY_FILE` or `ENC_PASSPHRASE` (app-layer defense-in-depth)
- Offline: 127.0.0.1 / intranet only, no cloud

## [1.0.1] — 2026-08-28

### Added
- B-2 auto-failover e2e integration test (first orbit-chain coverage).

### Changed
- 12 crates 1.0.0 → 1.0.1 (patch, no API change).
- README + CLAUDE.md test counts 424 → 425.

## [1.0.0] — 2026-08-28

### Added
- C-1 API 1.0 + version freeze (11 crates 0.2.1 → 1.0.0, semver commitment).
- B-1 static encryption (AES-256-GCM field encryption, FDE + app-layer).
- B-2 auto-failover election (lean self-contained, no openraft).
- C-2 fuzz + load bench (fm-fuzz + retrieve_bench 100k).
- Deploy artifacts (systemd/launchd/Dockerfile), /metrics, backup, 8MB 413.

### Gates
- 424 offline tests green, clippy -D warnings, fmt, check clean.
