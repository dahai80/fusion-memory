# Changelog

All notable changes to fusion-memory. Format: Keep a Changelog. SemVer 2.0.0.
Internal path-dep private ecosystem (not on crates.io); versions tag + GitHub release.

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

1. **Perf baseline = StubEmbedder**. retrieve_bench 100k uses stub (no mlx HTTP latency). Real bge-m3 latency not measured. Index-layer perf only.
2. **store-stub naming**. hnsw_rs + sled backend, long-term production per README, but named "stub". Documented as production backend, not temporary.
3. **Upstream items (tracked, not fusion-memory code)**:
   - fusion-store #3/#4 — zero-copy backend (fusion-memory uses store-stub)
   - fusion-guard #2 — formal DLP PII gate (fg-redact covers credentials only; fusion-memory has interim redact.rs)
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
