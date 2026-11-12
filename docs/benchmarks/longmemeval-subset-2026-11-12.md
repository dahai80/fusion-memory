---
doc_id: fusion-memory/guide/longmemeval-subset
product: fusion-memory
class: core
entry: service
doc_type: guide
title: { zh: "LongMemEval 子集基线 (M4)", en: "LongMemEval subset baseline (M4)" }
audience: engineer
status: draft
version: 1.6.0
updated: 2026-11-12
owner: @dahai
source_of_truth:
  - crates/fm-engine/tests/longmemeval_subset.rs
  - docs/benchmarks/longmemeval-subset-baseline.json
---

# LongMemEval 子集基线 (M4, issue #41)

LongMemEval-shaped synthetic corpus: 20 needle instances, each = one evidence session
(needle fact: "The maintenance access code for {codename} is {code}.") + one near-miss
session (same keywords, no answer — lexical adversarial) + 6 filler sessions. Runs through
the REAL engine path (`commit_episodic_memory` → `retrieve_context`, four-way fusion).

Metrics: evidence recall@5, MRR. CI gate: never-below committed baseline
(`docs/benchmarks/longmemeval-subset-baseline.json`, stub mode; `-live.json`, bge-m3).

## Repro (defense line 6)

```bash
# stub mode (CI, offline, ~1s)
cargo test -p fm-engine --test longmemeval_subset -- --nocapture
# live mode (real bge-m3 via fusion-mlx)
cargo test -p fm-engine --features mlx-live --test longmemeval_subset -- --include-ignored
# regenerate baseline (verify no regression before committing)
FM_LME_UPDATE=1 cargo test -p fm-engine --test longmemeval_subset
```

## Baseline 2026-11-12 (v1.6.0)

| mode | instances | recall@5 | MRR |
|------|-----------|----------|-----|
| stub | 20 | 1.0000 | 1.0000 |
| live bge-m3 | 20 | 1.0000 | 1.0000 |

Ranking shape: evidence rank 1, near-miss rank 2, fillers below — both embedder modes.

## Notes

- This is a pipeline gate on LongMemEval-shaped data, not the full LongMemEval benchmark
  (question-type coverage is out of scope here); full-benchmark adoption tracked in the
  evolution plan.
- Live model: `BAAI/bge-m3` via fusion-mlx `:11434` (`FUSION_MEMORY_EMBED_MODEL`).
- Score-order finding recorded during bring-up: FTS term extraction must strip trailing
  punctuation (a "?" glued onto a codename starves the BM25 arm) and the FTS arm needs 4x
  candidate oversampling — both fixed in v1.6.0.
