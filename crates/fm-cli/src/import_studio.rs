//! import-agent-studio: 从 ~/.fusion-agent-studio/memory.db 迁移记忆。PRD §11.5。
//!
//! 源库 SQLite + FTS5, 三级 short_term/long_term/archive, memory_type user/feedback/project/reference。
//! 映射:
//! - tier: short_term→Short, long_term→Long, archive→skip (冷数据不污染活跃记忆)。
//! - memory_type: user→Semantic, feedback→Procedural, project→Episodic, reference→Semantic。
//! - importance(0-10)→weight: clamp(i/10, 0.1, 1.0)。
//! - 实体: scope `graph:NAME` 或 metadata.graph_id → Project EntityNode (确定性, 不走 LLM, Rule 5)。
//! - embedding: 真 MlxEmbedder (dim 1024); --stub 降级 StubEmbedder (离线测试)。
//! - created_at(REAL epoch 秒)→created_timestamp(ms)。
//! - is_summary=1 → provenance="imported-summary"。

use std::sync::Arc;

use fm_core::{EntityNode, EntityType, MemoryItem, MemoryTier, MemoryType};
use fm_embed::{EmbedConfig, Embedder, MlxEmbedder, StubEmbedder};
use fm_engine::MemoryEngine;
use fm_persist::Persist;
use fm_store::LocalStore;
use rusqlite::Connection;
use tracing::{info, warn};

/// 导入报告。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ImportReport {
    pub imported: usize,
    pub skipped_archive: usize,
    pub skipped_empty: usize,
    pub failed: usize,
}

/// 源库一行 (镜像 agent-studio memories 表)。
#[derive(Debug, Clone)]
pub struct StudioRow {
    pub id: String,
    pub content: String,
    pub scope: String,
    #[allow(dead_code)]
    pub tags: String,
    pub importance: i64,
    pub created_at: f64,
    pub metadata: String,
    pub is_summary: i64,
    pub tier: String,
    pub memory_type: String,
}

/// C16 关键词归类 (移植 agent-studio classify_memory_type)。user 优先于 feedback,
/// reference (URL/票据) 与前两者正交故末, 无命中默认 project。
/// 返回源库 memory_type 字符串 (user/feedback/project/reference), 供 map_memory_type。
pub fn classify_memory_type(content: &str) -> &'static str {
    let text = content.to_ascii_lowercase();
    let user_kw = [
        "i am",
        "i'm",
        "my role",
        "i use",
        "i work",
        "i prefer",
        "expertise",
    ];
    let feedback_kw = [
        "prefer",
        "don't",
        "always use",
        "never use",
        "should",
        "rule:",
        "why:",
        "how to apply",
    ];
    let reference_kw = [
        "http://",
        "https://",
        "url:",
        "dashboard",
        "ticket",
        "doc at",
        "see ",
    ];
    if user_kw.iter().any(|k| text.contains(k)) {
        return "user";
    }
    if feedback_kw.iter().any(|k| text.contains(k)) {
        return "feedback";
    }
    if reference_kw.iter().any(|k| text.contains(k)) {
        return "reference";
    }
    "project"
}

/// 源 tier → fusion MemoryTier。archive → None (跳过, 冷数据不进活跃记忆)。
pub fn map_tier(src: &str) -> Option<MemoryTier> {
    match src {
        "short_term" => Some(MemoryTier::Short),
        "long_term" => Some(MemoryTier::Long),
        "archive" => None,
        _ => Some(MemoryTier::Short),
    }
}

/// 源 memory_type → fusion MemoryType。
/// user→Semantic (身份偏好, 持久事实), feedback→Procedural (工作方式),
/// project→Episodic (进行中工作上下文), reference→Semantic (外部资源指针, 事实)。
pub fn map_memory_type(src: &str) -> MemoryType {
    match src {
        "user" | "reference" => MemoryType::Semantic,
        "feedback" => MemoryType::Procedural,
        _ => MemoryType::Episodic,
    }
}

/// importance(0-10) → weight [0.1, 1.0]。
pub fn importance_to_weight(importance: i64) -> f64 {
    let raw = importance as f64 / 10.0;
    raw.clamp(0.1, 1.0)
}

/// scope `graph:NAME` 或 metadata.graph_id → Project EntityNode (确定性, Rule 5, 不走 LLM)。
pub fn entity_from_scope(scope: &str, metadata_json: &str) -> Option<EntityNode> {
    let name = if let Some(rest) = scope.strip_prefix("graph:") {
        if rest.is_empty() {
            None
        } else {
            Some(rest.to_string())
        }
    } else {
        metadata_graph_id(metadata_json)
    }?;
    let slug = slugify(&name);
    // L5: id = ent-{slug}-{fnv1a(name)}: slug 仅显示, hash 保唯一 (与 entity_extract.rs 同方案)。
    // 避免 slug("C")=="c"==slug("C++")==slug("C#") 三实体共享 ent-c 碰撞, 污染图谱对齐。
    Some(EntityNode {
        id: format!("ent-{slug}-{:016x}", fnv1a_64(name.as_bytes())),
        name,
        aliases: Vec::new(),
        entity_type: EntityType::Project,
    })
}

/// 从 metadata JSON 取 graph_id (宽松解析, 失败返 None)。
fn metadata_graph_id(metadata_json: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(metadata_json).ok()?;
    let g = v.get("graph_id")?.as_str()?.to_string();
    if g.is_empty() {
        None
    } else {
        Some(g)
    }
}

fn slugify(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

/// FNV-1a 64bit。确定性 hash, 保实体 id 稳定 (同名同 id, 异名异 id)。与 entity_extract.rs 同实现。
fn fnv1a_64(data: &[u8]) -> u64 {
    let mut h = 0xcbf29ce484222325u64;
    for &b in data {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// 读源库全部行 (排除 FTS 虚表)。
pub fn read_studio_rows(source_db: &str) -> Result<Vec<StudioRow>, String> {
    let conn = Connection::open(source_db).map_err(|e| format!("open source db: {e}"))?;
    let mut stmt = conn
        .prepare(
            "SELECT id, content, scope, tags, importance, created_at, metadata, is_summary, \
             tier, memory_type FROM memories",
        )
        .map_err(|e| format!("prepare: {e}"))?;
    let rows = stmt
        .query_map([], |row| {
            Ok(StudioRow {
                id: row.get(0)?,
                content: row.get(1)?,
                scope: row.get(2)?,
                tags: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
                importance: row.get(4).unwrap_or(5),
                created_at: row.get(5).unwrap_or(0.0),
                metadata: row
                    .get::<_, Option<String>>(6)?
                    .unwrap_or_else(|| "{}".into()),
                is_summary: row.get(7).unwrap_or(0),
                tier: row
                    .get::<_, Option<String>>(8)?
                    .unwrap_or_else(|| "short_term".into()),
                memory_type: row
                    .get::<_, Option<String>>(9)?
                    .unwrap_or_else(|| "project".into()),
            })
        })
        .map_err(|e| format!("query_map: {e}"))?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r.map_err(|e| format!("row: {e}"))?);
    }
    Ok(out)
}

/// 执行导入: 读行 → 映射 → embed → 入库。
pub async fn run_import(engine: &MemoryEngine, source_db: &str) -> Result<ImportReport, String> {
    let rows = read_studio_rows(source_db)?;
    info!(rows = rows.len(), "studio rows read");
    let mut report = ImportReport::default();
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    for row in rows {
        let tier = match map_tier(&row.tier) {
            Some(t) => t,
            None => {
                report.skipped_archive += 1;
                continue;
            }
        };
        let content = row.content.trim();
        if content.is_empty() {
            report.skipped_empty += 1;
            continue;
        }
        // R8/§10.4: 导入路径 PII 脱敏 (与 commit 共用 env FUSION_MEMORY_REDACT_PII)。
        let content = if import_redact_on() {
            let r = fm_engine::redact_text(content);
            if r != content {
                info!(id = %row.id, "PII redacted on import");
            }
            r
        } else {
            content.to_string()
        };
        let content = content.as_str();
        // memory_type: 源库已有字段; 空或无效 → 按内容重新归类 (C16)。
        let src_type = if VALID_STUDIO_TYPES.contains(&row.memory_type.as_str()) {
            row.memory_type.as_str()
        } else {
            classify_memory_type(content)
        };
        let memory_type = map_memory_type(src_type);
        let weight = importance_to_weight(row.importance);
        let created_ts = (row.created_at * 1000.0) as u64;
        let entity = entity_from_scope(&row.scope, &row.metadata);
        let entities: Vec<EntityNode> = entity.into_iter().collect();

        let vec = match engine.embedder().embed(content).await {
            Ok(v) => v,
            Err(e) => {
                warn!(id = %row.id, error = %e, "embed failed, skip row");
                report.failed += 1;
                continue;
            }
        };
        let vec_id = fm_embed::vector_id_from_ulid(&row.id);
        if let Err(e) = engine.store().insert_vector(vec_id, &vec) {
            warn!(id = %row.id, error = %e, "store insert failed");
            report.failed += 1;
            continue;
        }
        let item = MemoryItem {
            id: row.id.clone(),
            interaction_id: format!("studio-{}", row.id),
            turn_idx: 0,
            session_id: "import-agent-studio".to_string(),
            tenant: String::new(),
            memory_type,
            tier,
            content: content.to_string(),
            entities,
            vector_ref: vec_id.to_string(),
            weight,
            access_count: 0,
            last_accessed_timestamp: now_ms,
            created_timestamp: created_ts,
            provenance: if row.is_summary != 0 {
                Some("imported-summary".into())
            } else {
                Some("imported-agent-studio".into())
            },
            tombstone: false,
            entities_pending: false,
        };
        if let Err(e) = engine.persist().put_memory(&item) {
            warn!(id = %row.id, error = %e, "persist failed");
            report.failed += 1;
            continue;
        }
        report.imported += 1;
    }
    info!(
        imported = report.imported,
        skipped_archive = report.skipped_archive,
        skipped_empty = report.skipped_empty,
        failed = report.failed,
        "import done"
    );
    Ok(report)
}

const VALID_STUDIO_TYPES: [&str; 4] = ["user", "feedback", "project", "reference"];

/// 构造导入用 engine (真 MlxEmbedder dim=1024; --stub 降级 StubEmbedder 离线测试)。
pub fn build_import_engine(home: &Option<String>, stub: bool) -> Result<MemoryEngine, String> {
    let dir = crate::paths::resolve_home(home);
    std::fs::create_dir_all(&dir).map_err(|e| format!("create home dir: {e}"))?;
    let store_dir = dir.join("store");
    let db_path = dir.join("memory.db");
    let dim = if stub { 64 } else { 1024 };
    let store =
        Arc::new(LocalStore::open(&store_dir, dim).map_err(|e| format!("store open: {e}"))?);
    let persist = Arc::new(Persist::open(&db_path).map_err(|e| format!("persist open: {e}"))?);
    let embedder: Arc<dyn Embedder> = if stub {
        Arc::new(StubEmbedder::new(dim))
    } else {
        let cfg = EmbedConfig {
            api_key: std::env::var("FUSION_MEMORY_MLX_API_KEY").unwrap_or_default(),
            ..EmbedConfig::from_env()
        };
        let mlx = MlxEmbedder::new(cfg).map_err(|e| format!("mlx embedder: {e}"))?;
        Arc::new(mlx)
    };
    let mut engine = MemoryEngine::new(store, persist, embedder);
    if import_redact_on() {
        engine = engine.with_redact();
    }
    Ok(engine)
}

/// R8: 导入路径是否脱敏。与 commit 路径共用 env FUSION_MEMORY_REDACT_PII, 保持一致语义。
fn import_redact_on() -> bool {
    fm_engine::redact_enabled_env()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;

    #[test]
    fn classify_user_priority_over_feedback() {
        // "i prefer X" → user (不命中裸 prefer→feedback)
        assert_eq!(classify_memory_type("I prefer Rust"), "user");
        assert_eq!(classify_memory_type("i am a backend engineer"), "user");
        assert_eq!(classify_memory_type("expertise in ml"), "user");
    }

    #[test]
    fn classify_feedback() {
        assert_eq!(
            classify_memory_type("always use 4 space indent"),
            "feedback"
        );
        assert_eq!(
            classify_memory_type("never use print for debug"),
            "feedback"
        );
        assert_eq!(classify_memory_type("rule: commit in english"), "feedback");
    }

    #[test]
    fn classify_reference() {
        assert_eq!(classify_memory_type("see https://example.com"), "reference");
        assert_eq!(classify_memory_type("ticket: JIRA-123 bug"), "reference");
    }

    #[test]
    fn classify_default_project() {
        assert_eq!(classify_memory_type("ran the pipeline today"), "project");
        assert_eq!(classify_memory_type(""), "project");
    }

    #[test]
    fn map_tier_short_long_archive() {
        assert_eq!(map_tier("short_term"), Some(MemoryTier::Short));
        assert_eq!(map_tier("long_term"), Some(MemoryTier::Long));
        assert_eq!(map_tier("archive"), None);
        assert_eq!(map_tier("bogus"), Some(MemoryTier::Short));
    }

    #[test]
    fn map_memory_type_variants() {
        assert_eq!(map_memory_type("user"), MemoryType::Semantic);
        assert_eq!(map_memory_type("reference"), MemoryType::Semantic);
        assert_eq!(map_memory_type("feedback"), MemoryType::Procedural);
        assert_eq!(map_memory_type("project"), MemoryType::Episodic);
        assert_eq!(map_memory_type("unknown"), MemoryType::Episodic);
    }

    #[test]
    fn importance_weight_bounds() {
        assert!((importance_to_weight(10) - 1.0).abs() < 1e-9);
        assert!((importance_to_weight(0) - 0.1).abs() < 1e-9);
        assert!((importance_to_weight(-5) - 0.1).abs() < 1e-9);
        assert!((importance_to_weight(7) - 0.7).abs() < 1e-9);
        assert!((importance_to_weight(100) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn entity_from_scope_graph_prefix() {
        let e = entity_from_scope("graph:douyin_auto_publish", "{}").unwrap();
        assert_eq!(e.entity_type, EntityType::Project);
        assert_eq!(e.name, "douyin_auto_publish");
        assert!(e.id.starts_with("ent-"));
        assert!(e.aliases.is_empty());
    }

    #[test]
    fn entity_from_scope_metadata_graph_id() {
        let e = entity_from_scope("default", r#"{"graph_id":"90bd7634da9445a3"}"#).unwrap();
        assert_eq!(e.name, "90bd7634da9445a3");
    }

    #[test]
    fn entity_id_no_slug_collision_l5() {
        // L5: C / C++ / C# slug 均为 "c" 但 id 必须不同 (fnv1a hash 区分)。
        let c = entity_from_scope("graph:C", "{}").unwrap();
        let cpp = entity_from_scope("graph:C++", "{}").unwrap();
        let csharp = entity_from_scope("graph:C#", "{}").unwrap();
        assert_ne!(c.id, cpp.id, "C 与 C++ id 必须不同");
        assert_ne!(c.id, csharp.id, "C 与 C# id 必须不同");
        assert_ne!(cpp.id, csharp.id, "C++ 与 C# id 必须不同");
        // 同名确定性: 同名同 id。
        let c2 = entity_from_scope("graph:C", "{}").unwrap();
        assert_eq!(c.id, c2.id, "同名实体 id 应确定一致");
    }

    #[test]
    fn entity_from_scope_none() {
        assert!(entity_from_scope("default", "{}").is_none());
        assert!(entity_from_scope("graph:", "{}").is_none());
        assert!(entity_from_scope("graph:", "not json").is_none());
    }

    #[allow(clippy::type_complexity)]
    fn make_source_db(path: &str, rows: &[(&str, &str, &str, i64, f64, &str, i64, &str, &str)]) {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            "CREATE TABLE memories (id TEXT PRIMARY KEY, content TEXT, scope TEXT, tags TEXT, \
             importance INTEGER, created_at REAL, metadata TEXT, is_summary INTEGER, \
             tier TEXT, memory_type TEXT);",
        )
        .unwrap();
        for (id, content, scope, imp, created, meta, summ, tier, mtype) in rows {
            conn.execute(
                "INSERT INTO memories(id,content,scope,tags,importance,created_at,metadata,\
                 is_summary,tier,memory_type) VALUES(?1,?2,?3,'',?4,?5,?6,?7,?8,?9)",
                params![id, content, scope, imp, created, meta, summ, tier, mtype],
            )
            .unwrap();
        }
    }

    #[test]
    fn read_studio_rows_parses() {
        let dir = std::env::temp_dir().join(format!("fm-import-read-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("src.db");
        make_source_db(
            db.to_str().unwrap(),
            &[
                (
                    "r1",
                    "hello rust",
                    "default",
                    7,
                    1700000000.0,
                    "{}",
                    0,
                    "short_term",
                    "project",
                ),
                (
                    "r2",
                    "i prefer python",
                    "graph:svc",
                    9,
                    1700000100.0,
                    "{}",
                    1,
                    "long_term",
                    "user",
                ),
                (
                    "r3",
                    "old cold",
                    "default",
                    2,
                    1700000000.0,
                    "{}",
                    0,
                    "archive",
                    "project",
                ),
                (
                    "r4",
                    "",
                    "default",
                    5,
                    1700000000.0,
                    "{}",
                    0,
                    "short_term",
                    "project",
                ),
            ],
        );
        let rows = read_studio_rows(db.to_str().unwrap()).unwrap();
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[0].id, "r1");
        assert_eq!(rows[1].memory_type, "user");
        assert_eq!(rows[2].tier, "archive");
        assert_eq!(rows[3].content, "");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn run_import_stub_end_to_end() {
        let home_dir = std::env::temp_dir().join(format!("fm-import-e2e-{}", std::process::id()));
        let src_dir = std::env::temp_dir().join(format!("fm-import-src-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home_dir);
        let _ = std::fs::remove_dir_all(&src_dir);
        std::fs::create_dir_all(&src_dir).unwrap();
        let db = src_dir.join("src.db");
        make_source_db(
            db.to_str().unwrap(),
            &[
                (
                    "m1",
                    "hello rust world",
                    "graph:douyin",
                    7,
                    1700000000.0,
                    "{}",
                    0,
                    "short_term",
                    "project",
                ),
                (
                    "m2",
                    "i am a data scientist",
                    "default",
                    9,
                    1700000100.0,
                    "{}",
                    0,
                    "long_term",
                    "user",
                ),
                (
                    "m3",
                    "archived cold data",
                    "default",
                    2,
                    1700000000.0,
                    "{}",
                    0,
                    "archive",
                    "project",
                ),
                (
                    "m4",
                    "   ",
                    "default",
                    5,
                    1700000000.0,
                    "{}",
                    0,
                    "short_term",
                    "project",
                ),
                (
                    "m5",
                    "ref https://hf-mirror.com",
                    "default",
                    6,
                    1700000200.0,
                    "{}",
                    0,
                    "short_term",
                    "reference",
                ),
            ],
        );
        let engine =
            build_import_engine(&Some(home_dir.to_string_lossy().to_string()), true).unwrap();
        let report = run_import(&engine, db.to_str().unwrap()).await.unwrap();
        // m1(project/short), m2(user/long), m5(reference/short) 导入; m3 archive skip; m4 empty skip
        assert_eq!(report.imported, 3, "imported {:?}", report);
        assert_eq!(report.skipped_archive, 1);
        assert_eq!(report.skipped_empty, 1);
        assert_eq!(report.failed, 0);
        assert_eq!(engine.persist().count().unwrap(), 3);
        let all = engine.persist().list_all().unwrap();
        // m2 long_term → Long tier, user → Semantic
        let m2 = all.iter().find(|m| m.id == "m2").unwrap();
        assert_eq!(m2.tier, MemoryTier::Long);
        assert_eq!(m2.memory_type, MemoryType::Semantic);
        assert!((m2.weight - 0.9).abs() < 1e-9, "importance 9 → 0.9");
        assert_eq!(m2.provenance.as_deref(), Some("imported-agent-studio"));
        // m1 graph:douyin → Project entity
        let m1 = all.iter().find(|m| m.id == "m1").unwrap();
        assert_eq!(m1.tier, MemoryTier::Short);
        assert_eq!(m1.memory_type, MemoryType::Episodic);
        assert_eq!(m1.entities.len(), 1);
        assert_eq!(m1.entities[0].entity_type, EntityType::Project);
        // m5 reference → Semantic
        let m5 = all.iter().find(|m| m.id == "m5").unwrap();
        assert_eq!(m5.memory_type, MemoryType::Semantic);
        let _ = std::fs::remove_dir_all(&home_dir);
        let _ = std::fs::remove_dir_all(&src_dir);
    }

    #[tokio::test]
    async fn build_import_engine_stub_creates_dirs() {
        let home = std::env::temp_dir().join(format!("fm-import-build-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let engine = build_import_engine(&Some(home.to_string_lossy().to_string()), true).unwrap();
        assert!(home.exists());
        assert_eq!(engine.store().dimension(), 64);
        let _ = std::fs::remove_dir_all(&home);
    }
}
