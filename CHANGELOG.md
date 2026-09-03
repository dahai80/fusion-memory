# Changelog

All notable changes to fusion-memory. Format: Keep a Changelog. SemVer 2.0.0.
Internal path-dep private ecosystem (not on crates.io); versions tag + GitHub release.

## [Unreleased]

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
