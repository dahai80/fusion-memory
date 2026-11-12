---
doc_id: fusion-memory/guide/retrieval-quality-m1-fourway
product: fusion-memory
class: core
entry: service
doc_type: guide
title: { zh: "四路融合检索消融报告 (M1)", en: "Four-way retrieval ablation report (M1)" }
audience: engineer
status: draft
version: 1.6.0
updated: 2026-11-12
owner: @dahai
source_of_truth:
  - crates/fm-engine/tests/retrieval_quality.rs
---

# 四路融合检索消融报告 (M1, issue #39)

Repro (defense line 6 — numbers always ship with a repro command):

```bash
cargo test -p fm-engine --test retrieval_quality -- --nocapture          # gate scale
FM_RQ_SCALE=full cargo test -p fm-engine --test retrieval_quality full_scale_run -- --nocapture
```

Corpus: synthetic, deterministic seed `0x5eed_25ab_cd10_0001`, dim=64. Per topic: 5 fresh
(relevant, topic entity, content carries topic codename), 5 stale (same vector, 30-60d old,
no codename), 3 distract (mixed vectors), 2 lexical-only (relevant, vector from an unrelated
topic — semantically unreachable, content carries the codename; the structured replication
of a proper-noun/codename lexical miss). All candidates persisted via `put_memory` so the
FTS5 trigram index is exercised for real.

## Gate-scale results (40 topics, 520 items, 40 queries)

| arm | recall@5 | nDCG@10 | MRR |
|-----|----------|---------|-----|
| plain cosine | 0.2750 | 0.4558 | 0.6148 |
| cosine+decay | 0.4179 | 0.6172 | 0.8375 |
| cosine+graph | 0.5143 | 0.6577 | 0.8362 |
| full fusion (three-way, α/β/γ=0.5/0.3/0.2) | 0.5821 | 0.7229 | 0.9125 |
| fusion raw-W (pre-retune, deprecated) | 0.2143 | 0.3335 | 0.7611 |
| **four-way (M1, 0.4/0.3/0.2/0.1)** | **0.6786** | **0.9748** | **1.0000** |

Lexical recovery (lexical-only relevant in top-10): four-way **40/40**, plain cosine **0/40**.

## Full-scale results (400 topics, 5200 items, 400 queries)

| arm | recall@5 | nDCG@10 | MRR |
|-----|----------|---------|-----|
| plain cosine | 0.2496 | 0.3978 | 0.5735 |
| full fusion (three-way) | 0.5443 | 0.6767 | 0.8823 |
| **four-way (M1)** | **0.6779** | **0.9668** | **0.9933** |

## Verdict

- Gate chain holds with strict improvements: four-way (0.6779) > three-way (0.5443) > cosine
  (0.2496) on recall@5 at full scale; CI gates `ablation_four_way_never_below` and
  `lexical_hits_recovered_by_four_way` enforce never-below going forward.
- BM25 arm contributes +0.134 recall@5 over three-way at full scale — entirely from
  semantically-unreachable lexical hits (plain cosine recovers 0/40).

## Known limitations

- FTS5 trigram tokenizer cannot match terms shorter than 3 characters (substring minimum);
  short common words stay covered by the semantic path. Keyword path targets codenames,
  proper nouns and long terms.
- Deployments with field encryption (B-1, `ENC_KEY_FILE`) keep no plaintext token index:
  FTS sync is skipped on write and `fts_search` returns empty (fail-safe), so the BM25 arm
  contributes nothing there. Cosine/graph/freshness arms unaffected.
