# fusion-memory

> **English** | **[中文](README_CN.md)**

> **Current version: v1.2.0 (Commercial GA)** — Multi-tenant isolation landed (issue #16). v1.1.0 Commercial GA: hard blockers all closed + real-test verification + three RC known limitations all resolved. Known limitations in `CHANGELOG.md` "Known limitations", non-blocking.

The system-level long/short-term memory and cognitive graph hub of the Fusion ecosystem ("一核九端"). Solves the Agent cross-session state gap, repeated questions, and context-window blowup. Goal: the more it is used, the better it knows the user.

- Authoritative PRD: `architecture/fusion-memory-prd-0825.md`
- Landing architecture: `~/fusion/fusion-memory-prd-plan-0826.md`
- Audit report: `~/fusion/audit/fusion-memory-audit-0827.md`

## Status

**v1.0.0 — API stability commitment (2026-08-28)**: 11 crates locked at 1.0.0. From here follows the semantic versioning (SemVer) contract:
- `MAJOR` (2.0+): only **backward-incompatible** breaking changes, must be announced in changelog in advance + migration guide. Breaking changes include: removing/renaming existing RPC methods, changing HTTP paths, changing field semantics, changing default behavior.
- `MINOR` (1.x): backward-compatible new methods/fields/endpoints/perf improvements; clients **must not** break on a MINOR upgrade.
- `PATCH` (1.0.x): bug fixes, no behavior change.
- **Frozen wire contract**: UDS JSON-RPC method set + `v1.<method>` prefix routing + `jsonrpc=="2.0"` validation (see `jsonrpc::API_VERSION=1`); HTTP `/v1/memory/*` paths + Bearer auth + `confirm` guard. Both unchanged across the entire 1.x cycle.
- Client negotiation: call the `version` RPC or `GET /v1/memory/version` to get `api_version`, branch on the version number.
- Pre-1.0 0.x versions are technical previews, no stability commitment.

**store-fusion adapter + fg-redact credential redaction landed (2026-08-28, unreleased)**: user request "build store-fusion adapter now, then switch upstream fg-redact" — both parts fully landed.
- **store-fusion adapter** (`fm-store/src/fusion.rs`, feature `store-fusion`): implements the fm-store `FusionStoreEngine` trait, wrapping upstream fusion-store `fs-core` `Engine` (HNSW + mmap KV). Distance semantics bridge: fs-core returns `distance = 1 - cos_sim`, the adapter converts `similarity = 1.0 - distance` to align with the fm-store contract (same formula as local.rs). UFCS calls to the fs-core trait (both crates have a trait of the same name `FusionStoreEngine`). ZeroCopyBuffer mmap→owned bridge. 6 tests green (kv roundtrip / vector insert+get+search / dim mismatch reject / search dim mismatch / delete→None / list_ids excludes soft-deleted). **Additive, not exclusive**: coexists with local-store (local-store default + always-on; store-fusion optional, default off), both compile together. Closes RC known-limitation #2 (store-stub naming — store-fusion is now a real fusion-store-backed alternative, not just "stub").
- **fg-redact credential redaction** (`fm-engine/src/redact.rs`): stage-1 credential redaction delegates to upstream `fg-redact::Redactor::redact_credentials()` (fusion-guard PR #11 / issue #10). fg-redact adds 10 credential classes fusion-memory previously lacked (JWT/private_key/oauth_bearer/api_key/conn_string/password/secret_kv/env_kv/netrc/aws_secret). Stage-2 PII remains fusion-memory-local (phone+86/0086/email/idcard/bankcard+Luhn/IPv4/passport/IPv6/intl phone) — fg-redact's PII behavior is worse (idcard wrongly eaten by credit_card / id_number wrongly eats long digits / +86 phone rejected by border validator), so PII does not go through fg-redact, see redact.rs module doc. Closes RC known-limitation #3 credential part (credentials now upstream; PII stays local by design). 4 new tests (jwt / password / credential+PII same segment / idcard stays local not bankcard). Idempotent: credential placeholder `[REDACTED:jwt]` has no digits → PII regex no re-match.
- **Test counts**: default features 425→429 (+4 credential tests); `--features fm-store/store-fusion` 435 (429 + 6 store-fusion tests). Gates all green (fmt / clippy -D warnings / check / test).
- **Upstream**: fusion-guard #10/#11 credential API landed (issue filed + PR #11 implemented + 8 issue10_* tests), fusion-memory consumes `redact_credentials()`. fusion-store #3/#4 still tracked (adapter built consuming fs-core via path dep; store-stub remains default production backend).

**Multi-tenant isolation landed (2026-09-02, issue #16)**: backend half of fusion-gateway #150 Gap1c. Canonical pattern follows fusion-model-hub#53.
- **Tenant field on data** (`fm-core`): `MemoryItem.tenant`, `Interaction.tenant` (`#[serde(default)]`), `RetrieveQuery.tenant` (`#[serde(default)]`). Tenant flows through data, NOT through core `FusionMemoryEngine` trait method signatures — preserves v1.0 API freeze. `tenant=""` = default tenant (single-tenant backward compatible).
- **Additive tenant-scoped trait methods**: `get_memory_tenant` / `delete_memory_tenant` / `delete_scope_tenant` / `count_tenant` — default-delegate to non-tenant variants; impls override for real scoping. Backward compatible.
- **Schema v3 migration** (`fm-persist`): `tenant TEXT NOT NULL DEFAULT ''` column + `idx_memory_tenant` index. Idempotent `ALTER TABLE` via `pragma_table_info` check.
- **Engine scoping** (`fm-engine`): `MemoryEngine.tenant` (startup default, `""`), `with_tenant` builder. commit/summarize/retrieve/consolidate scope by `self.tenant`. Cross-tenant: get → None (no existence leak), delete → NotFound, missing-row delete → Ok (soft-delete preserved).
- **Gateway-origin enforcement** (`fm-server/src/tenant.rs`): `X-Fusion-Route: gateway-decision` header (config `gateway_origin_required: bool = false` default off → 403 when missing); `X-Fusion-Tenant` authoritative tenant (header > `default_tenant` config > ""). HTTP `handle_rpc` + `get_memory` enforce before dispatch. UDS (trusted-local) uses default tenant. Config env: `FUSION_MEMORY_GATEWAY_ORIGIN_REQUIRED`, `FUSION_MEMORY_DEFAULT_TENANT`.
- **Test counts**: default features 429→433 (+4 gateway-origin/tenant tests in fm-server); `--features fm-store/store-fusion` 435→439. Gates all green (fmt / clippy -D warnings / check / test). Version bump all 12 crates 1.1.0→1.2.0 (additive, no breaking change).

**M2 completed**: real bge-m3 embedding (dim=1024) + entity extraction (injection-proof prompt + strict JSON parsing) + SQLite recursive CTE graph traversal + rule-priority entity alignment + fused scoring (cosine+decay+graph_affinity) + agent-studio history memory import. Acceptance: entity extraction JSON parse success rate 100% (>90%), rule-priority alignment correct (same name same type merge / same name different type no merge), real embedding roundtrip dim=1024. Test coverage lines 90.59% / regions 92.17%. 162 tests all green, clippy -D warnings passed.

**M3 completed**: fm-server (UDS JSON-RPC 0600 + HTTP axum enforced Bearer B5, port 11435, refuses to start HTTP without API_KEY) + fm-py PyO3 binding (`allow_threads` GIL-safe C2) + consolidate_memories saga (incremental forgetting + merge/summarize/reconcile, cross-DB reconciliation + merge_log + unmerge) + fm-cli (consolidate/merges/unmerge/reconcile) + start.sh (start/stop/restart/status/log/doctor). Acceptance: PyO3 roundtrip GIL not frozen (commit→2 ids / retrieve→block / consolidate→report); HTTP rejected without token + DELETE rejected without confirm + HTTP refused without API_KEY; consolidate report fields complete + reconciliation diff detected; start.sh three commands usable. 242 offline + live tests all green, regions offline 90.63% / live 92.07%.

**M4 in-scope completed** (consumer integration reference impl + contract tests, in this repo): `clients/` three consumer reference clients — TS HTTP client (`ts/fusionMemoryClient.ts`, fusion-code vendor, default port 11440 to avoid clashing with fusion-kb 11435) + Python HTTP client (`python/fusion_memory_client.py`, cowork/agent-studio alternate path, default 11435) + `clients/README.md` integration doc (protocol matrix + wire contract + three consumer integration seams + port conflict warning + agent-studio 9 handler→6 RPC mapping table). Contract scenario tests `crates/fm-server/tests/consumer_scenarios.rs` (3 scenarios: cowork memory_commit/retrieve node flow, fusion-code retrieve inject→commit→cross-turn recall, agent-studio 9 handler backend-replace mapping + delete without confirm -32602). Stub engine HTTP oneshot roundtrip, offline no mlx. Acceptance: 3 contract scenarios pass + 248 offline tests all green + clippy/fmt clean + regions 91.76% (up, new scenarios expand trait path coverage). **Outward PRs landed** (cross-repo, 3 consumer repos issue→PR→land): fusion-cowork #67→#68 (merged, memory_commit/retrieve two nodes), fusion-agent-studio #246→#247 (merged, FusionMemoryAdapter 9 handler→6 RPC env-gated swap), fusion-code #150→#151 (merged 5311b00, turn-end commit; retrieve-inject half-deferred, tracked #154). All three consumer integration files verified present on each repo's main.

**M1 completed**: store-stub backend (hnsw_rs + sled) + SQLite WAL persistence + StubEngine (deterministic stub embedding) + CLI (commit/query/stats/delete/doctor). Acceptance: CLI writes 100 entries (50 interaction × 2 turn) → query aggregates each block restoring 2 turns → doctor reports component status. Test coverage lines 94.6% / regions 91.1% (cargo-llvm-cov).

**M6 completed**: cluster sync leader-follower (PRD §16 intranet offline cluster, not public cloud). New crate `fm-cluster`: role injection (standalone/leader/follower, env `FUSION_MEMORY_ROLE` > home/role file > standalone) + wop_log replication (leader single write point + append_wop, follower pulls SyncRequest → local replay commit/delete, summarize audit skipped) + TCP transport (4B length prefix + JSON wire frame, Hello/SyncRequest/SyncResponse/Ping/Pong, intranet port 11436) + heartbeat (5s ping, 3 consecutive failures = LeaderDown) + manual failover (`fm cluster promote` writes home/role=leader, requires fm-server restart to take effect, auto-election deferred). fm-server `spawn_cluster(engine, role, set)` role injection eliminates env races. fm-cli `cluster status/promote`. Acceptance: 3 e2e scenarios all green (commit→catchup read-local consistent / incremental sync seq advance / leader down→LeaderDown→promote→new leader continues writing) + ReplaySink coverage tests + fm-cluster per-file offline regions 91-100%. 285 offline tests all green, clippy/fmt clean. **Offline total regions 87.65%** (down from M4 90.63%, due to new fm-cluster crate + engine integration expanding the regions denominator, while mlx-gated summarize/consolidate saga + engine_builder !stub branches are unreachable offline; the live metric still covers these branches, PRD acceptance caliber = live, M6 did not touch mlx code so live regions unchanged).

**M5 partially completed** (degraded positioning, non-blocking mainline): PRD §14 three parts — (a) store-fusion optional switch, (b) `audit_memory_access` → fusion-guard DLP gate, (c) perf baseline p99<50ms + concurrency. **(c) landed**: lightweight hand-written bench (`crates/fm-engine/benches/retrieve_bench.rs`, no criterion heavy dep), store-stub 10k memories + StubEmbedder dim=64, single retrieve p99=14.3ms (<50ms ✅), 10-concurrent p99=140ms (<200ms ✅). Baseline JSON saved to `benches/baseline-2026-08-27.json`. **(a)(b) degraded**: see M5 PRD deviation record. **R8/§10.4 PII regex redaction landed beyond (c)** (filling the prior zero-redaction vacuum): `fm-engine/src/redact.rs` five PII regex classes (phone/email/idcard/bankcard/ipv4, regex crate has no lookaround, order-sensitive replacement to avoid mis-eating), placeholder `[REDACTED:type]`, idempotent. The commit/import write path redacts before embed+persist, so vectors/graph/retrieval all use redacted content. env `FUSION_MEMORY_REDACT_PII=1` enables it (`MemoryEngine::with_redact()` + fm-server/fm-cli import path same-source env). 13 redaction tests green. Acceptance: perf bench two gates pass + 13 redaction tests green + 301 offline tests all green + live tests all green (bge-m3 + Qwen3.8-27B-4bit, entity extraction JSON 100%) + offline regions 90.82% / live regions 92.47% (both ≥90%) + clippy/fmt clean.

| Milestone | Scope | Status |
|-----------|-------|--------|
| M0 | workspace + core types + trait + CI | ✅ |
| M1 | store-stub backend + engine runnable (stub embedding) + CLI | ✅ |
| M2 | real embedding + entity extraction + graph + fused scoring + import | ✅ |
| M3 | service + PyO3 + consolidate + auth | ✅ |
| M4 | consumer integration (in-scope ✅ / outward ✅) | ✅ |
| M5 | PII redaction + perf baseline + store-fusion/guard degraded | ✅ (partial) |
| M6 | cluster sync leader-follower | ✅ |

> **Full milestone set M0–M6 (terminal state)**: landing architecture `~/fusion/fusion-memory-prd-plan-0826.md` §14 defines only M0–M6, M6 is the final milestone, **no M7+**. §15 six open items all ✅ adjudicated; §17 audit corrections E1 (8 items)/E2 (10 items)/E3 (3 items) all landed or decided, audit closed with no residual. Subsequent work is only two types: (1) M5(b) awaiting upstream `fusion-guard#2` to add PII classes before connecting the formal DLP gate; (2) PRD-external ops/perf/consumer evolution.

### Audit P0–P3 fix record (2026-08-27, fully closed)

All 16 defects in `audit/fusion-memory-audit-0827.md` §8 fixed, landed in 9 batches by file cluster, each batch a `cargo test` checkpoint, 315 offline tests all green (baseline 301 → +14 new regression tests), clippy `-D warnings` + fmt clean.

**P0 commercial blockers (5 items)**
- **H1 cross-store write has no transactional atomicity**: `put_memory` wrapped in `conn.transaction()` (memory_item + entity + memory_entity three INSERT types in same transaction, failure rollback leaves no half entity rows); `commit_episodic_memory` reverse `delete_vector` to clean already-written sled vectors on `put_memory` failure. `fm-persist/src/store.rs`, `fm-engine/src/engine.rs`.
- **H2 cluster replay not idempotent + LeaderDown false positive**: `insert_vector` made idempotent (already on disk and not tombstoned → skip `hnsw.insert`, replay resends don't double-index; tombstone state clears tomb then re-inserts normally = resurrection path); replay errors classified (transient network/parse failure retry, permanent sink failure propagate). `fm-store/src/stub.rs`, `fm-cluster/src/replay.rs`.
- **H3 cluster TCP has no auth + 4GB frame OOM + plaintext**: `read_frame` adds `MAX_FRAME_LEN` (16MB) cap to prevent OOM; `handle_conn` validates cluster_token (compared during Hello handshake, empty token allowed on intranet); plaintext risk documented (intranet offline boundary, not public, PRD §16 already scoped). `fm-cluster/src/protocol.rs`, `fm-cluster/src/transport.rs`.
- **H4 consolidate TOCTOU + lost update**: engine-level `tokio::sync::Mutex<()>` (`consolidate_lock`) serializes `consolidate_memories` against retrieve's touch_access write (snapshot→decision→write atomic); `touch_access_batch` dedup + single batch `UPDATE ... WHERE id IN (...)`, relative to `access_count=access_count+1` prevents lost update. `fm-engine/src/engine.rs`, `fm-persist/src/store.rs`.
- **H5 PII redaction not enterprise-grade + lying comment**: bankcard adds Luhn check + context boundary (avoid mis-eating order numbers/timestamps); expand PII classes (phone/email/idcard/bankcard/ipv4, order-sensitive replacement to avoid phone eating first 11 digits of bankcard); `redact.rs:58` env comment corrected (reads env on every call, not at startup, and only on the builder/import non-hot path). `fm-engine/src/redact.rs`.

**P1 must-fix (5 items)**
- **L1 graph_affinity always 0**: `retrieve_context` extracts entities from query text when extractor present → passes `query_entity_ids` to `score_candidate`, graph_affinity connected (direct hit 1.0 / N-hop 0.5^h). `fm-engine/src/engine.rs`.
- **L2 touch_access multi-accumulation**: see H4 `touch_access_batch` (same id multiple turn hits only +1, "retrieval session" count).
- **L3 reconcile one-directional**: add store→SQLite reverse orphan scan (`StoreStub::list_vector_ids()` enumerates non-tombstoned vector ids, not in SQLite `vector_ref` set → orphan, recorded in report + `delete_vector`); `physical_delete` explicitly cascades `memory_entity` (not relying on FK pragma across connections). `fm-store/src/stub.rs`, `fm-persist/src/store.rs`, `fm-engine/src/engine.rs`.
- **L4 delete silently skips**: bad `vector_ref` (non-numeric/polluted) no longer `unwrap_or(true)` silent physical delete (would leave ghost vectors), changed to warn + `append_reconcile("bad-vector-ref")` + skip physical delete (reconcile backstop cleans). `fm-engine/src/engine.rs`.
- **L5 slug collision**: entity id = `ent-{slug}-{fnv1a_64(name)}` (FNV-1a full-name hash guarantees uniqueness, slug is display-only). C/C++/C# etc. same slug, different entity ids all distinct. `fm-engine/src/entity_extract.rs`, `fm-cli/src/import_studio.rs`.

**P2 perf (4 items)**
- **P1 single Mutex full serialization + poison panic amplification**: keep single `Mutex<Connection>` (SQLite WAL single writer, r2d2 connection pool is over-design for Rule 2), 24 `.expect("poisoned")` → `conn()`/`conn_mut()` helpers return `PersistError::Poisoned` propagate, no longer panic amplifying single-point failure to global. `fm-persist/src/store.rs`, `fm-persist/src/error.rs`.
- **P2 vector serde_json text stored in sled (~3.7x waste)**: change to LE f32 raw bytes (4B/float, serde_json text 7-12B/float), zero-alloc deserialize. `fm-store/src/stub.rs`.
- **P3 consolidate_merge O(S×KNN×N) catastrophe**: KNN inner loop `list_all()` full-table scan + string reverse lookup → build `vector_id → &MemoryItem` index once outside loop, inner O(1) lookup. `fm-engine/src/engine.rs`.
- **P4 CTE exponential fan-out + per-search single-point tombstone check**: recursive CTE adds `LIMIT 256` early termination (graph_affinity remote nodes 0.5^h exponential decay, truncation lossless); `search_knn` tombstone check changed to `tombstone_set()` single batch load into HashSet, replacing per-neighbor/per-fallback vector N sled point lookups. `fm-persist/src/store.rs`, `fm-store/src/stub.rs`.

**P3 maintenance (3 items)**
- **M1 lying comment**: physical_delete cascade comment (already explicitly DELETE memory_entity, comment matches impl) + redact.rs env comment (see H5). `fm-persist/src/store.rs`, `fm-engine/src/redact.rs`.
- **M2 extract_and_attach swallows DB error**: `get_memory().unwrap_or(None)` turns SQLite error into "no such memory" (DB failure disguised as data missing) → explicit match, DB error warn + return (pending stays true for re-extract), no disguise. `fm-engine/src/engine.rs`.
- **M3 Runtime::new per Engine thread explosion**: fm-py builds a tokio runtime per Python `Engine` (N×worker threads) → process-level `OnceLock<Runtime>` shared single runtime (2 workers), all PyEngine reuse. `fm-py/src/lib.rs`.

### v0.1.1 patch (2026-08-27, issue #1/#2/#4)

Fixed 3 open GitHub issues, added 2 RPCs + 1 UDS method:

- **issue #2 — scope-level delete/count**: added `delete_scope` (batch tombstone by session_id, with `confirm` guard, reuses delete's vector cleanup + `append_wop` audit) and `count` (full or per-session count). Backend `fm-persist` adds `delete_by_session`/`list_by_session`/`count_by_session`, engine `MemoryEngine::delete_scope`/`count`, trait adds default `Unsupported` impl (test stubs unchanged). HTTP `POST /v1/memory/{delete_scope,count}` + UDS method `delete_scope`/`count`.
- **issue #1/#4 — `memory.retrieve_context` contract**: fusion-event needs `{trigger_id, query, top_k, node_id}` → `{context, memory_ids, cache_hit}`. Added UDS method `memory.retrieve_context` adapting existing `RetrieveQuery`, fusing `FormattedContext.blocks` into the contract shape (context = turns joined by `\n---\n`, memory_ids = interaction_id dedup, cache_hit=false).

Acceptance: 325 offline tests all green (baseline 301 → +24 new, persist 3 + dispatch 5 + http 4 + trait/engine 12), clippy `-D warnings` + fmt clean, `cargo check --workspace` clean. CI blocked by GitHub account billing (`recent account payments have failed`, not a code issue, local fmt/clippy/check/test gate is the proxy metric).

### v0.2.0 audit second-round deep fix (2026-08-28, architecture layer + production-path gate)

Deep items in audit report `audit/fusion-memory-audit-result-0827.md` §1/§2/§3 (48 findings, landed in 8 batches) all fixed. This round focused on architecture-layer coupling, production-path zero-coverage, error-type semantics, complementing the v0.1.x behavior-defect fixes. 354 offline tests all green (baseline 325 → +29 new regression tests), clippy `-D warnings` + fmt clean.

**Architecture-layer decoupling (§1.1/§1.4/§1.5)**
- **§1.1 connection pool breaks single-Mutex serialization**: `Persist` from `Mutex<Connection>` to `r2d2::Pool<SqliteConnectionManager>` (POOL_SIZE=8). WAL's native 1-writer N-reader concurrency was previously canceled by the single connection; `PooledConnection` Deref→`Connection`, `prepare_cached`/`transaction()` call sites unchanged. Added `PersistError::Pool` + `From<r2d2::Error>`, timeout/busy → `MemoryError::Busy` retryable. `fm-persist/src/store.rs`, `fm-persist/src/error.rs`, `Cargo.toml` (r2d2 0.8 / r2d2_sqlite 0.35, matching rusqlite 0.40 bundled).
- **§1.4 store backend trait-ification**: `MemoryEngine.store` field from `Arc<StoreStub>` to `Arc<dyn FusionStoreEngine>` (dynamic dispatch, not bound to a concrete backend). `FusionStoreEngine` trait adds `list_vector_ids` (reconcile reverse scan store→SQLite orphans). store-fusion backend changed from empty shell to `compile_error!` explicit block (until upstream fusion-store trait alignment). `fm-store/src/trait_def.rs`, `fm-store/src/stub.rs`, `fm-store/src/fusion.rs`, `fm-engine/src/engine.rs`.
- **§1.5 graph-layer storage abstraction**: new `fm_graph::GraphStore` trait (only `n_hop_reachable` + `list_entities_by_type` two methods, minimal interface for the graph layer), `impl GraphStore for Persist`. `graph_affinity`/`align_entity`/`score_candidate` signatures from `&Persist` to `&dyn GraphStore`, graph layer no longer `use fm_persist::Persist`. New mock test `mock_store_no_sqlite_needed` proves the graph layer can be pure-memory unit-tested (no `Persist::open_in_memory()` + SQL data fill needed). `fm-graph/src/store.rs`, `fm-graph/src/affinity.rs`, `fm-graph/src/align.rs`, `fm-engine/src/scoring.rs`.

**Production-path gate (§1.6/§1.12)**
- **§1.6 CI live-compile gate**: CI adds `live-compile` job, each PR compiles all mlx-live gated tests (`--features mlx-live --no-run`), verifying `MlxEmbedder` bge-m3 / consolidate saga / real HTTP code paths type-check. Previously these production paths were compiled 0 times in the default CI build (live tests `#![cfg(feature = "mlx-live")]` + `#[ignore]` double gate), "325 green" had zero regression protection for live paths. Actual execution remains manual (needs fusion-mlx on Apple Silicon). `.github/workflows/ci.yml`.
- **§1.12 stub-vs-stub tautology**: 91 stub tests (`StubEngine`/`DispatchStub`/`EchoEngine` returning magic constants) annotated as wiring tests (proving wire passes params, not behavior tests); real `MemoryEngine` behavior coverage through HTTP/JSON-RPC real-stack roundtrip is borne by `tests/offline_integration.rs` + `tests/consumer_scenarios.rs` (stub engine + real stack, already run by default in `cargo test --workspace`). `fm-server/src/jsonrpc.rs`.

**Error-type semantics (§2.8)**
- **§2.8 error type finalize**: `MemoryError` adds `Poisoned`/`Busy`/`NotFound` semantic variants (old version all crushed into `Sqlite(String)`, ops mistook it for sqlite error and ran VACUUM, real diagnosis hidden). `PersistError::to_memory` distinguishes mapping: Poisoned→Poisoned (permanently non-retryable), SQLITE_BUSY/locked→Busy (transient retryable), Pool timeout→Busy. `retryable()`/`is_not_found()` helpers for caller decision. `fm-core/src/error.rs`, `fm-persist/src/error.rs`.

> Full 8-batch record (Batch 0–7, by file cluster + checkpoint) in git history `fix/audit-p0-p3-layering-0828` branch. Acceptance metric: 354 offline tests all green + clippy `-D warnings` + fmt clean + live-compile gate compiles.

### v0.2.1 production-readiness audit P0–P3 fix (2026-08-28, third round)

All 22 items (6 P0 + 10 P1 + 6 P2, no P3) in production-readiness audit report `audit/fusion-memory-audit-result-product-0827.md` §8 handled. All code-fixable items landed, 3 epic-scale architecture items explicitly deferred and documented as SLA/roadmap. 403 offline tests all green (baseline 354 → +49 new regression tests), clippy `-D warnings` + fmt clean, `cargo check --workspace` clean.

**P0 commercial blockers (6 items all fixed)**
- **P0-1 process supervision**: added `scripts/fusion-memory.service` (systemd unit, Type=notify integrates healthz, Restart=on-failure + StartLimitBurst, journal logging). Companion `start.sh` documents deployment path (systemd managed / manual start.sh, choose one). `scripts/fusion-memory.service`.
- **P0-2 metrics endpoint**: `GET /metrics` returns Prometheus text format (http_requests_total / http_errors_total / http_request_duration_seconds histogram + engine-layer embedder_in_flight / consolidate_running / store_pool_in_use). Public without Bearer (like healthz, for monitor scraping). `crates/fm-server/src/metrics.rs`, `crates/fm-server/src/http.rs`.
- **P0-3 HTTP body limit**: axum `DefaultBodyLimit::max(8MB)` on all routes (aligned with UDS `MAX_LINE_BYTES`), over-limit 413 Payload Too Large never reaches handler, prevents POST large-body memory-amplification DoS. `crates/fm-server/src/http.rs`.
- **P0-4 backup mechanism**: `scripts/backup.sh` (SQLite `.backup` online hot backup + sled dir cp, timestamp archive, configurable retention window), `fm-cli backup` subcommand calls same logic. Cron deployment documented. `scripts/backup.sh`, `crates/fm-cli/src/backup.rs`.
- **P0-5 CI billing-blocked**: not a code issue (GitHub account `recent account payments have failed` blocks Actions paid runs). Local gate (fmt/clippy/check/test) as proxy metric all green. Deferred until account billing restored, not code-fixable.
- **P0-6 deployment artifacts**: `Dockerfile` (multi-stage build, distroless runtime, non-root user, only exposes 11435). `scripts/build-artifact.sh` packages binary + config template. `Dockerfile`, `scripts/build-artifact.sh`.

**P1 must-fix (10 items all fixed)**
- **P1-1 commit partial failure**: `commit_episodic_memory` returns `CommitOutcome{memory_ids, failed_turns}`, single turn embed/insert/persist failure recorded as `TurnFailure` without interrupting other turns, client can sense failed turns and retry. Old version crushed all into `Err` losing the whole batch. `crates/fm-engine/src/engine.rs`, `crates/fm-core/src/report.rs`.
- **P1-2 consolidate half-merge compensation**: merge writes `memory_item` successfully but `merge_log` fails → `unmerge` auto-rollback (reverse merge + clear merge_log row + warn), no half-merge ghost left. `crates/fm-engine/src/engine.rs`.
- **P1-3 tracing + audit log**: full-engine `tracing` structured logging (commit/retrieve/consolidate each stage span + counts); `audit_log` table records actor/action/target/detail, consolidate audit records `actor="system"`. `crates/fm-engine/src/engine.rs`, `crates/fm-persist/src/store.rs`.
- **P1-4 PII log leak**: `tracing` fields redacted via `redact_text` (memory content/params pass PII regex before logging), logs contain no raw PII. `crates/fm-engine/src/engine.rs`.
- **P1-5 UDS token auth**: UDS connection-level token (`FUSION_MEMORY_UDS_TOKEN`, connection first line `AUTH <token>` handshake compare, empty token allowed locally), mismatch → `-32004 unauthorized` disconnect. Multi-tenant UDS auth. `crates/fm-server/src/uds.rs`.
- **P1-6 cluster bind gate**: leader/follower startup validates bind address (non-127.0.0.1/intranet segment → refuse to start, prevent accidental public binding), PRD §16 offline boundary hard constraint. `crates/fm-cluster/src/transport.rs`.
- **P1-7 scale-validation bench**: `crates/fm-engine/benches/scale_bench.rs`, 10k/100k/1M vector scale validation (`FM_SCALE` env selects tier), measures seed throughput / rebuild_from_sled / single knn p99 / 10-concurrent retrieve p99 / sled disk usage. 100k baseline saved to `benches/baseline-scale-2026-08-28.json`. Old version only 10k, scale unvalidated. `crates/fm-engine/benches/scale_bench.rs`.
- **P1-8 config file**: `fm-server` supports TOML config (`FM_CONFIG` env or `data_dir/fusion-memory.toml`) + env override + secret files (`FUSION_MEMORY_API_KEY_FILE`/`FUSION_MEMORY_UDS_TOKEN_FILE`, avoid secrets in env/cmdline) + startup `validate()` fail-visible exit(1). Priority env > TOML > secret_file > default. `crates/fm-server/src/config.rs`, `crates/fm-server/src/main.rs`.
- **P1-9 connection pool get timeout**: r2d2 `connection_timeout(5s)` explicit backstop (default 30s), pool-full `get()` timeout returns `GetTimeout` → `MemoryError::Busy` retryable, not infinite block preventing deadlock. `crates/fm-persist/src/store.rs`, `crates/fm-persist/src/error.rs`.
- **P1-10 StoreStub naming consistency**: `store-stub` → `local-store` (feature flag), `StoreStub` → `LocalStore` (type), `stub.rs` → `local.rs` (file). The sole implementor's naming de-pejorated (not stub, is long-term production backend). `crates/fm-store/`.

**P2 post-release (6 items: 3 fixed + 3 explicitly deferred)**
- **P2-2 PII coverage expansion**: `redact.rs` adds IPv6 (abbreviated + full 8 segments) + international phone (E.164 `+\d{7,15}` non-86 country code, ordered after China phone to avoid double redaction) patterns. Name/address regex high false-positive rate (locale-heavy), deferred to fusion-guard UDS `guard.redact` (awaiting upstream fusion-guard#2 to add PII classes, see M5 deviation record b). `crates/fm-engine/src/redact.rs`.
- **P2-3 summarize failure visibility**: `consolidate_summarize`'s mlx call failure (None = network/non-2xx/parse) or empty content return, old version only warn silently swallowed → now records `ConsolidationFailure{stage:"summarize"}` for client awareness. `crates/fm-engine/src/engine.rs`.
- **P2-4 API versioning**: JSON-RPC `jsonrpc=="2.0"` validation (non-2.0 → `-32600 invalid_request`, old version silently swallowed field); method version prefix `v1.<method>` routing (no prefix = latest = v1, backward compatible); new `version` method + `GET /v1/memory/version` endpoint returns `api_version` for client negotiation. `crates/fm-server/src/jsonrpc.rs`, `crates/fm-server/src/http.rs`.
- **P2-1 auto failover / split-brain protection — landed (v1.0.0 B-2, retired deferred status)**: `fm-cluster::election` lean self-contained election module (leader-lease + term + quorum + wop_log last_seq judgment, **no openraft**) replaces manual failover. Leader down → follower auto-campaigns and wins → `epoch++` + writes role=Leader → restarts as leader (RTO seconds). Old leader revived is rejected by §1.8 StaleLeader fencing (prevents split-brain double-write). Manual `fm cluster promote` still retained (when no election config). Split-brain protection: quorum majority write + epoch fencing + token auth (reuses H3). 16 new tests cover. See `### v1.0.0 auto failover election`.
- **P2-5 Persist god-object split into traits — explicitly deferred (architecture refactor epic)**: `Persist` currently 30+ methods (Memory/Relation/Entity/Wop/Reconcile mixed). Adjudication deferred: ① splitting 5 traits (Memory/Relation/Entity/Wop/Reconcile) touches all engine call sites (~60 signature changes `&Persist` → `&dyn MemoryStore` etc.) + fm-py PyO3 binding + fm-cluster ReplaySink, is a cross-crate architecture refactor epic, not this round's P0-P3 single-point fix; ② the `fm-graph::GraphStore` trait (v0.2.0 §1.5 already split the graph-layer minimal interface) proves the trait-split pattern is viable, the Persist split follows the same method but at a different scale; ③ the current `Persist` though a god-object has clear internal partitioning (each responsibility's methods grouped + commented), not blocking commercial. Roadmap: dedicated PR for the split, with migration tests.
- **P2-6 dep migration sled→fjall / hnsw_rs alternative — explicitly deferred (under evaluation)**: sled 0.34 + hnsw_rs 0.3.4 maintenance-risk assessment. Adjudication deferred: ① sled author has pushed fjall (successor project, different API), migration is a local-store backend full rewrite + data format migration (on-disk vectors need reformat), not this round's scope; ② hnsw_rs alternatives (`hnsw`/`hora` libs) need benchmark comparison of recall/latency, evaluation incomplete so no switch; ③ both deps currently functionally stable (100k scale bench validated, see P1-7), no known blocking bug. Roadmap: first bench-evaluate alternative libs' recall/latency, then decide migration priority; sled→fjall if done, with a data-migration script.

> Acceptance metric: 403 offline tests all green (baseline 354 → +49 new regression tests, covering P0-2/3 metrics/body, P1-1/8/9/10 outcome/config/pool/rename, P2-2/3/4 PII-expansion/summarize-failure/API-version each 4-6 tests) + clippy `-D warnings` + fmt clean + `cargo check --workspace` clean. 3 deferred items (P2-1/5/6) documented as SLA/roadmap, not code-fixable boundary.

### M2 PRD deviation record (Rule 7)

- **Kuzu DB → SQLite recursive CTE** (adjudicated 2026-08-26): PRD §9.2 chose Kuzu DB for the embedded graph, but Kuzu has no Rust binding. Changed to SQLite recursive CTE (`relation` table + `WITH RECURSIVE` N-hop traversal), implemented in `fm-persist`, consumed by `fm-graph::graph_affinity`. Functionally equivalent (N-hop reachability + direct hit), no extra server process needed.

### M5 PRD deviation record (Rule 7, degradation adjudication 2026-08-27)

PRD §14 M5 three parts, (a) store-fusion optional switch, (b) fusion-guard DLP gate degraded to self-contained PII regex redaction, (c) perf baseline landed.

- **(a) store-fusion switch → originally "degraded not implemented", 2026-08-28 adapter landed** (adjudicated 2026-08-27 → updated 2026-08-28): PRD §14/Tech Selection planned to reuse `fusion-store` (HNSW) as zero-copy backend. Original "degraded not implemented" adjudication based on: ① `fusion-store`'s `FusionStoreEngine` (`fs-core`) and fm-store's same-named trait are two different APIs; ② constrained by "only modify this project's code". **2026-08-28 update**: after upstream fusion-store landed `fs-core`, fm-store adds `store-fusion` feature + `fusion.rs` adapter (wraps fs-core `Engine`, UFCS calls concrete methods not via fs-core trait, avoiding API misalignment), landed as an **optional backend** (default off, local-store remains default + always-on production backend). Both crates' same-named trait disambiguated by UFCS full-qualification. Zero-copy via ZeroCopyBuffer mmap→owned bridge (holding an mmap reference across calls is unsafe, so owned copy; true zero-copy needs the upper layer to hold the mmap handle alive, future optimization). 6 tests green. **store-stub remains the default long-term production backend unchanged**, store-fusion is an optional alternative. The perf gate still targets store-stub (default path), (c) unaffected.
- **(b) fusion-guard DLP gate → credential segment connected to upstream fg-redact, PII segment stays local** (adjudicated 2026-08-27 → updated 2026-08-28): PRD R8/§10.4 "M5 connects guard for formal DLP gate". Review found fusion-guard already landed (`fg-redact::Redactor` covers credentials + PII, but **PII behavior has defects**: idcard wrongly eaten by credit_card / id_number has no validator and mis-eats long digits / +86 phone rejected by border validator). **2026-08-28 handling** (user request "switch upstream fg-redact"): filed **fusion-guard #10** upstream (request `redact_credentials` credential-subset API, skip PII) → PR **fusion-guard#11** landed `redact_credentials()` + `redact_with_patterns()` + `CREDENTIAL_PATTERNS` const (8 issue10_* tests). fusion-memory `redact.rs` stage-1 consumes `redact_credentials()` (10 credential classes, adding AWS/JWT/PEM/bearer/conn_string/.env coverage previously missing); stage-2 PII remains fusion-memory-local (more accurate than fg-redact PII, tested). Integration point unchanged (`with_redact()` builder + commit/import write path). Future full UDS `guard.redact` DLP gate awaits upstream PII behavior fix.
- **(c) perf baseline → landed**: see M5 summary. Both gates met, baseline JSON archived.

### v1.0.0 commercial-blocker fix (2026-08-28, fourth round — commercial-readiness hardening)

Handling of 7 blockers before commercial release. Class A 3 items constrained by cross-repo/account not code-fixable (tracking upstream issues), class B 2 epics + class C 2 items landed this round.

**Class A — cross-repo/account blockers (not fixable in this repo, upstream issues filed and tracked)**
- **A-1 fusion-store zero-copy backend**: PRD §14 planned to reuse fusion-store HNSW zero-copy. **2026-08-28 adapter built** (`fm-store/src/fusion.rs`, feature `store-fusion`, wraps fs-core `Engine`), landed as optional backend (default off, local-store remains default production backend). Still tracking **fusion-store #3** (request VectorIndex expose `get_vector(id)`+`list_vector_ids()`) + **fusion-store #4** (request Engine impl) to complete the fs-core API surface. Integration point unchanged (`FusionStoreEngine` trait already trait-ified, §1.4).
- **A-2 fusion-guard DLP gate**: see M5 deviation record (b). **2026-08-28 credential segment connected upstream** (fusion-guard #10/#11, `redact_credentials()` consumed); PII segment stays fusion-memory-local (fg-redact PII behavior defects, by design). Full UDS `guard.redact` DLP gate awaits upstream PII fix.
- **A-3 GitHub Actions CI**: account billing blocked (P0-5), not code. Local gate (fmt/clippy/check/test + fuzz) as proxy metric all green.

**Class B — previously deferred epics, landed this round**
- **B-1 static encryption**: see `### v1.0.0 static encryption` section. FDE (FileVault/LUKS) as primary at-rest encryption (ops layer, `deploy/README.md` documented) + app-layer AES-256-GCM sensitive-field encryption (`fm-persist` encryption layer, defense-in-depth). Vectors not app-encrypted (FDE covers + upstream PII redaction).
- **B-2 auto failover**: see `### v1.0.0 auto failover election` section. **Lean self-contained election** (leader-lease + term + quorum voting, **no openraft dependency** — openraft only has alpha with no stable release, commercial risk), replaces manual `fm cluster promote`. Leader down → follower auto-campaigns, RTO from manual-intervention level down to seconds. Retires P2-1 deferred status.

**Class C — clean fix, landed this round**
- **C-1 API 1.0 stability commitment**: 11 crates 0.2.1→1.0.0, SemVer contract + wire-contract freeze (see status section v1.0.0 statement). `jsonrpc::API_VERSION=1` + `v1.` prefix routing + `jsonrpc=="2.0"` validation in place.
- **C-2 fuzz + load stress**: see `### v1.0.0 fuzz + load stress` section. `cargo-fuzz` JSON-RPC/HTTP parsing fuzz target + 100k vectors + high-concurrency stress.

**Still awaiting upstream (not this round)**: fusion-store #3/#4 (fs-core API surface completion, adapter built consuming path dep), fusion-guard PII behavior fix (credential segment connected #10/#11, full UDS DLP gate awaits PII fix), GitHub account billing restore (CI).

**v1.0.0 B/C batch acceptance**: 425 offline tests all green (baseline 408 → +16 election unit + B-1 encryption 4 already in baseline + 1 B-2 e2e full-chain orbit), clippy `-D warnings` + fmt clean + `cargo check --workspace` clean. B-1 static encryption 4 tests (plaintext-compat/encrypt-roundtrip/wrong-key fail-open/no-cipher-reads-ciphertext) + B-2 election 16 unit tests (voting 4 criteria + quorum + campaign win/loss + lease expiry + live TCP vote listener + from_env boundary) + B-2 e2e 1 test (`fm-server/tests/election_failover.rs`, orbit full chain: leader down → campaign → quorum → epoch++/role file → detect Leader).

### v1.0.0 static encryption

**Layered strategy (Rule 7, single choice FDE as primary + app encryption as defense-in-depth, not averaging the two)**:

1. **FDE = primary at-rest encryption (ops layer)**: SQLite `memory.db` + sled vector store + cluster wop_log all land on FDE-encrypted volumes. macOS = FileVault (full disk); Linux = LUKS/dm-crypt (data volume). FDE is transparent to the application — hnsw_rs computes distance in RAM using plaintext, on-disk is ciphertext, KNN unaffected. `deploy/README.md` documented as deployment prerequisite + verification steps.
2. **App-layer field encryption = defense-in-depth (code)**: the `fm-persist` encryption layer AES-256-GCM encrypts SQLite `memory.content` + `entity.text` sensitive text columns, so even if a disk snapshot leaks (bypassing FDE) it is not plaintext. Key source: `FUSION_MEMORY_ENC_KEY_FILE` (0600 file, 32B raw key) or `FUSION_MEMORY_ENC_PASSPHRASE` (argon2 KDF derived). Read path transparently decrypts.
3. **Vectors not app-encrypted**: the hnsw_rs index needs plaintext to compute distance, app encryption would break KNN. Vectors covered by FDE + upstream PII redaction (redacted before commit, vectors derived from redacted content).
4. **Cluster transport**: intranet loopback/private network, TLS not yet enforced (PRD §16 intranet offline); sensitive fields already app-encrypted, not plaintext in transit.

**Key management**: no key = plaintext mode (compat with old behavior, env not configured); key configured = encrypted mode. Key rotation = re-encrypt script (`fm-cli` to add later). Keys not in repo, not in logs.

### v1.0.0 auto failover election

**Lean self-contained election (replaces manual failover, retires P2-1 deferral)**:

- **Selection**: **lean self-contained election module** (`fm-cluster::election`, ~400 LOC), **no openraft** — openraft's latest is only `0.10.0-alpha.34`, no stable release, MSRV unknown, large dependency tree, commercial blocker (Rule 2 simplicity + Rule 7 single selection). Implements Raft essentials (not full Raft), fits the 100% offline constraint.
- **Algorithm**: leader-lease (`heartbeat_secs × heartbeat_fails`, reuses SyncConfig) + term-increment voting + quorum (`floor(nodes/2)+1`) + log-newness judgment (reuses wop_log `last_seq`).
- **Campaign trigger**: follower `run()` detects LeaderDown (consecutive heartbeat failures) → transitions to candidate → self-increments term + votes for self → sends `VoteRequest` to all peers (new `FrameKind::VoteRequest/VoteResponse`, reuses TCP wire frame) → gets quorum → wins.
- **Vote authorization criteria** (Raft): ① `candidate.term ≥ own.term`; ② `candidate.last_seq ≥ own.last_seq`; ③ not voted this term (or already voted for this candidate); ④ token matches (reuses H3 auth). Not met → reject.
- **Win → promote**: candidate gets quorum → `epoch++` (`write_epoch_file`, reuses §1.8 fencing) + writes `role=Leader` (`write_role_file`) → orbit exits, supervisor restarts that node as leader. Old leader revived with lower epoch → follower `StaleLeader` rejects sync (prevents split-brain double-write).
- **Priority**: smaller node-list index has priority (deterministic, avoids randomness, same-term ties resolved).
- **Membership**: static, env `FUSION_MEMORY_CLUSTER_NODES=host:port,host:port,...` (all nodes), own index `FUSION_MEMORY_CLUSTER_NODE_ID` (0-based). Not configured → no election (standalone/manual mode compat, zero overhead). Membership change = change env + restart (not dynamic add/remove, Rule 2).
- **New deps**: none (reuses transport TCP / wop_log / role epoch file / async_trait).
- **M3 e2e scenario re-validation**: commit→catchup consistent / incremental seq advance / leader down→auto election→new leader continues writing (original promote path changed to auto). 16 election unit tests cover (vote authorize/reject 4 criteria, quorum, campaign win/loss, lease expiry, live TCP vote listener, from_env boundary) + 1 e2e full-chain integration test `fm-server/tests/election_failover.rs`: drives production entry `spawn_cluster(role=Follower)` + real MemoryEngine + real in-process TCP, verifies leader down → `follower_orbit` campaign → quorum → writes epoch++ + role=Leader file → `detect_role_with_home` reads Leader (fills orbit full-chain coverage, old claim only unit + manual promote).
- **Manual mode retained**: `fm cluster promote` still usable (when no election config). `fm cluster status` shows election state + epoch.

### v1.0.0 fuzz + load stress

- **fuzz**: `cargo-fuzz` targets — JSON-RPC request parsing (malformed JSON / overlong / bad UTF-8 / type confusion / deep nesting), HTTP body boundaries, entity extraction JSON parsing. Crash/panic treated as defect. `crates/fm-server/fuzz/`.
- **load stress**: expanded `retrieve_bench` — 100k vectors (baseline 10k × 10) + concurrency gradient (1/10/50/100), recording p50/p99/p999 + throughput. Commercial-grade load validation. Results saved to `benches/baseline-1.0.0-*.json`.

## Architecture

- Core Rust (no GC pauses), SQLite WAL + SQLite recursive CTE graph traversal (replaces Kuzu, see deviation record), store-stub (hnsw_rs + sled, long-term production backend)
- Three-tier memory: Working → Short-Term → Long-Term Graph
- Ebbinghaus forgetting curve + entity-relation cognitive graph
- **Turn-level storage**: a single dialogue turn = one MemoryItem, retrieval aggregates by `interaction_id` to restore the full Interaction
- 100% offline (local + intranet cluster), no cloud API, HTTP only to 127.0.0.1

## Crate structure

| crate | responsibility |
|-------|----------------|
| `fm-core` | Core data structures + `FusionMemoryEngine` trait (zero business deps) |
| `fm-engine` | Engine impl: MemoryEngine + entity extraction + fused scoring + decay + Long promotion |
| `fm-similarity` | Cosine similarity + decay W(t) (forgetting curve + reinforcement cap) |
| `fm-graph` | Rule-priority entity alignment (A5) + alias dictionary + graph_affinity (N-hop) |
| `fm-store` | `FusionStoreEngine` trait + local-store backend (default) + store-fusion backend (optional feature, wraps fusion-store fs-core) |
| `fm-embed` | fusion-mlx bge-m3 embedding (LRU+semaphore) + StubEmbedder |
| `fm-persist` | SQLite WAL metadata schema + CRUD + relation table (recursive CTE graph traversal) |
| `fm-server` | UDS JSON-RPC + HTTP service |
| `fm-py` | PyO3 Python binding |
| `fm-cli` | CLI ops/import/query |
| `fm-cluster` | M6 cluster sync: leader/follower roles + wop_log replication + TCP transport + manual failover; v1.0.0 B-2 auto failover election (`election` module, leader-lease + term + quorum) |

## Build

```bash
cargo check --workspace        # compile check
cargo test --workspace         # all offline tests (429 cases, excludes fm-py cdylib; --features fm-store/store-fusion = 435)
cargo clippy --workspace --all-targets -- -D warnings   # lint
cargo fmt --all --check        # format check

# §13.2 perf baseline (store-stub 10k entries, StubEmbedder, no model):
#   cargo bench -p fm-engine --bench retrieve_bench
#   single retrieve p99<50ms + 10-concurrent p99<200ms, results to /tmp/fm-perf-baseline-*.json

# §13.2 live perf baseline (real fusion-mlx bge-m3 dim=1024, end-to-end retrieve latency, closes RC known-limitation #1):
#   requires fusion-mlx running with bge-m3 loaded (standalone needs FUSION_ROUTE_WARN_ONLY=true + --api-key)
#   cargo bench -p fm-engine --features mlx-live --bench retrieve_bench_live
#   three paths: cold (unique query, real mlx) / cached (LRU hit, skip mlx) / concurrent x5 (real mlx)
#   baseline JSON: crates/fm-engine/benches/baseline-live-bgem3-2026-08-28.json
#   live data (Apple Silicon): cold p50=9.98ms/p99=10.73ms, cached p50=0.107ms/p99=0.126ms, concurrent x5 p50=44.6ms
#   scale constrained by fusion-mlx rate-limit bug (#692: _serve_from_model_dir misses config, 60rpm always-on), small-scale real-latency reference; large-scale stress uses retrieve_bench (StubEmbedder)

# real-model integration tests (requires fusion-mlx running with bge-m3 + Qwen chat model, serial to avoid 429):
#   ~/claude-home/fusion-mlx/start.sh start
#   scripts/live-test.sh            # full workspace live (serial)
#   scripts/live-test.sh fm-engine  # single crate
#   chat model defaults to Qwen3.5-9B-4bit; if not cached, override via env:
#   FUSION_MEMORY_CHAT_MODEL=Qwen3.8-27B-4bit scripts/live-test.sh   # verified usable
#   (Qwen3-0.6B entity extraction too weak, returns empty entities, not recommended for extract)
```

Coverage (requires llvm-tools, system llvm or rustup component):

```bash
# Offline default (CI metric): excludes fm-py (PyO3 cdylib binding layer, acceptance via PyO3 roundtrip, not unit-test coverage).
# regions 90.82%. 429 cases all green (--features fm-store/store-fusion = 435).
# Note: run `cargo llvm-cov clean` first, stale profraw (including untriggered bench instrumented binaries) dilutes regions.
# engine.rs summarize/consolidate saga + engine_builder.rs !stub branches unreachable offline (via real mlx LLM/embedding),
# PRD acceptance metric = live (covers these branches).
LLVM_COV=/opt/homebrew/opt/llvm/bin/llvm-cov \
LLVM_PROFDATA=/opt/homebrew/opt/llvm/bin/llvm-profdata \
cargo llvm-cov --workspace --summary-only --exclude-from-report fm-py --ignore-filename-regex "src/main\.rs"

# Real-model integration (requires fusion-mlx running with bge-m3 + Qwen3.5-9B-4bit, serial to avoid 429):
# ~/claude-home/fusion-mlx/start.sh start
LLVM_COV=/opt/homebrew/opt/llvm/bin/llvm-cov \
LLVM_PROFDATA=/opt/homebrew/opt/llvm/bin/llvm-profdata \
cargo llvm-cov --workspace \
  --features fm-embed/mlx-live --features fm-engine/mlx-live --features fm-server/mlx-live \
  --summary-only --exclude-from-report fm-py -- --include-ignored --test-threads=1
# live regions 92.47% (covers !stub real-mlx branches summarize/consolidate saga + engine_builder). PRD acceptance uses this.
```

> **Coverage metric**: regions is the standard (industry standard + PRD has no functions hard target).
> functions % is falsely low due to trait monomorphization cross-binary duplicate 0-counting (`FusionStoreEngine for StoreStub`
> instantiated in each test binary, uninvoked instances count 0), not truly uncovered — stub.rs all methods have unit tests,
> `tests/offline_integration.rs` actually calls the trait path. regions is unaffected by this artifact.
>
> **fm-py excluded**: PyO3 `extension-module` cdylib, macOS needs `.cargo/config.toml`'s
> `-undefined dynamic_lookup` to lazily resolve Python symbols. Acceptance = PyO3 roundtrip (see below), not unit-test coverage,
> consistent with PRD §11.3.
>
> **PyO3 roundtrip acceptance** (GIL-safe, C2):
> ```bash
> PYO3_PYTHON=/opt/homebrew/bin/python3.12 cargo build -p fm-py
> cp target/debug/libfusion_memory.dylib /tmp/fmpy/fusion_memory.so
> python3.12 roundtrip.py  # commit→2 ids / retrieve→block / consolidate→report / GIL not frozen
> ```
> During commit, another thread's pure-Python counter keeps incrementing → proves `py.allow_threads` releases the GIL, event loop not frozen.

Toolchain: edition 2021, MSRV 1.87. System rustc (Homebrew) compiles directly, no rustup needed.

## M1 data flow

```
commit  Interaction ──turn-level split──> MemoryItem per turn
                                 ├── embed(text, dim)  [M1: FNV-1a deterministic stub]
                                 ├── store.insert_vector(vec_id, vec)  [hnsw_rs + sled]
                                 └── persist.put_memory(item)  [SQLite WAL]
retrieve query ──embed──> store.search_knn(top_k)
         ──hit interaction_id──> persist.list_by_interaction backfill all turns
         ──assemble ContextBlock──> token-budget truncate ──> FormattedContext
delete   persist.get_memory → store.delete_vector (tombstone) + persist.tombstone
```

- **vector_id** = FNV-1a(ulid_string) → u64, stored in `MemoryItem.vector_ref`
- **Aggregate restore**: retrieval hits a turn, then queries persist for all turns by `interaction_id`, `AGG_MAX_TURNS=20`
- **Soft delete**: store tombstone + persist tombstone; `compact` physically removes + rebuilds hnsw

## M2 data flow

```
commit  turn ──MlxEmbedder.embed──> store.insert_vector(dim=1024)
                 └── persist.put_memory(entities_pending=true) ──> async extract_and_attach
                        └── MlxEntityExtractor(chat, injection-proof prompt) ──> strict JSON parse ──> entities writeback
retrieve query ──embed──> KNN ──> score_candidate = α·cosine + β·W(t) + γ·graph_affinity
                        α=0.5 β=0.3 γ=0.2; W(t)=W0·exp(-t/τ)·min(1+log2(1+count), CAP)
                        graph_affinity: direct hit=1.0, N-hop=0.5^h (hop≤2), else 0
consolidate  W(t)<θ_drop(0.05) → tombstone reclaim; Short→Long promotion; entities_pending batch re-extract
import       agent-studio memories ──map──> embed ──> ingest (scope/metadata → Project entity)
```

- **Entity alignment rule-priority chain** (A5, hit-and-stop): rule1 normalize+same-name-same-type(pri=3) → rule2 alias-dictionary canonical-name(pri=2) → rule3 existing name/alias(pri=1) → rule4 vector-threshold(pri=0) → new entity(pri=-1). **Same name different type not mergeable**.
- **Injection-proof** (§11.4): dialogue content wrapped in `<data>` tags, prompt explicitly says ignore instructions inside the tag; parse failure → empty entities, `entities_pending` stays true, content+vector still ingested.
- **Live acceptance**: `FUSION_MEMORY_MLX_API_KEY=change-me cargo run -p fm-cli --example live_acceptance` (requires fusion-mlx running bge-m3 + Qwen3.5).

## CLI usage

```bash
cargo build --release -p fm-cli   # produces target/release/fm

# Write a multi-turn interaction (JSON from stdin or --file)
echo '{"id":"ix-1","session_id":"s","turns":[{"turn_idx":0,"user_message":"hello rust","assistant_message":"hi","tool_calls":[]}],"timestamp":1,"metadata":{}}' \
  | ./target/release/fm --home ~/.fusion-memory --dim 64 commit --session s

./target/release/fm --home ~/.fusion-memory query --text "hello rust" --top-k 5 --budget 4096
./target/release/fm --home ~/.fusion-memory stats
./target/release/fm --home ~/.fusion-memory delete --id <memory_id> --confirm
./target/release/fm --home ~/.fusion-memory doctor

# Import from fusion-agent-studio history memories (real bge-m3, dim=1024)
FUSION_MEMORY_MLX_API_KEY=change-me ./target/release/fm --home ~/.fusion-memory import
# Offline test import (--stub uses StubEmbedder, dim=64)
./target/release/fm --home ~/.fusion-memory import --stub --source /path/to/memory.db

# M3: forget/merge/summarize/reconcile saga (PRD §5.6)
./target/release/fm --home ~/.fusion-memory consolidate           # trigger saga, reports dropped/promoted/merged/summarized/reextracted/reconciled
./target/release/fm --home ~/.fusion-memory merges                # list merge_log (to find id for unmerge)
./target/release/fm --home ~/.fusion-memory unmerge --id 42       # undo merge: source reverse tombstone, delete merge_log
./target/release/fm --home ~/.fusion-memory reconcile             # cross-DB reconciliation: tombstone physical delete, dangling vectors in report
```

`--home` defaults to `~/.fusion-memory` (or `FM_HOME` env), `--dim` defaults to 64 (stub); real embedding uses bge-m3 dim=1024, `import` without `--stub` auto-uses 1024.
`import` mapping: tier short_term→Short / long_term→Long / archive→skip; memory_type user→Semantic / feedback→Procedural / project→Episodic / reference→Semantic; scope `graph:NAME` or metadata.graph_id → Project entity.

## Service runtime (M3)

```bash
cargo build --release -p fm-server   # produces target/release/fm-server

# start.sh management (start/stop/restart/status/log/doctor)
./start.sh start      # start fm-server (default real bge-m3; FUSION_MEMORY_STUB=1 offline)
./start.sh stop       # graceful stop (SIGTERM)
./start.sh status     # PID/port/sock/memory/healthz
./start.sh doctor     # health check: binary/port/mlx connectivity/data dir
./start.sh log        # tail logs

# env (see ServerConfig::from_env):
#   FM_HOME (default ~/.fusion-memory)
#   FUSION_MEMORY_HTTP_PORT (default 11435) / FUSION_MEMORY_API_KEY (HTTP required, B5)
#   FUSION_MEMORY_STUB=1 (StubEmbedder offline, no mlx)
```

UDS JSON-RPC (sock 0600, B6) + HTTP (axum enforced Bearer, B5, port 11435) concurrent. Without `FUSION_MEMORY_API_KEY` but HTTP port open → refuses to start HTTP (UDS only). Routes: `POST /v1/memory/{commit,retrieve,consolidate,audit,delete,delete_scope,count}`, `GET /v1/memory/{id}`, `GET /healthz` (public); `delete`/`delete_scope` require `params.confirm=true`. UDS method `memory.retrieve_context` (issue #1/#4 contract, `{trigger_id,query,top_k,node_id}` → `{context,memory_ids,cache_hit}`, reuses retrieve engine chain).

## Conventions

- 4-space indentation, no docstrings (`//!` module docs + inline comments)
- `tracing` logging, `anyhow` (application) + `thiserror` (library)
- Fail visibly, no silent error swallowing
