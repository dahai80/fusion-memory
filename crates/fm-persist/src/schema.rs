//! SQLite schema DDL。PRD §4.3, §8.4。

pub const PRAGMA_WAL: &str = "PRAGMA journal_mode=WAL";
pub const PRAGMA_BUSY: &str = "PRAGMA busy_timeout=5000";
pub const PRAGMA_SYNC: &str = "PRAGMA synchronous=NORMAL";
pub const PRAGMA_FK: &str = "PRAGMA foreign_keys=ON";

pub const MEMORY_ITEM_DDL: &str = "\
CREATE TABLE IF NOT EXISTS memory_item (\
  id TEXT PRIMARY KEY,\
  interaction_id TEXT NOT NULL,\
  turn_idx INTEGER NOT NULL,\
  session_id TEXT NOT NULL,\
  memory_type TEXT NOT NULL,\
  tier TEXT NOT NULL,\
  content TEXT NOT NULL,\
  vector_ref TEXT NOT NULL DEFAULT '',\
  weight REAL NOT NULL,\
  access_count INTEGER NOT NULL DEFAULT 0,\
  last_accessed_timestamp INTEGER NOT NULL,\
  created_timestamp INTEGER NOT NULL,\
  provenance TEXT,\
  tombstone INTEGER NOT NULL DEFAULT 0,\
  entities_pending INTEGER NOT NULL DEFAULT 1,\
  entities_json TEXT NOT NULL DEFAULT '[]'\
)";

pub const MEMORY_ITEM_DDL_INSERT: &str = "\
INSERT OR REPLACE INTO memory_item(\
  id,interaction_id,turn_idx,session_id,memory_type,tier,content,vector_ref,weight,\
  access_count,last_accessed_timestamp,created_timestamp,provenance,tombstone,entities_pending,entities_json\
) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)";

pub const MEMORY_ITEM_DDL_SELECT_BY_ID: &str = "SELECT * FROM memory_item WHERE id=?1";

pub const ENTITY_DDL: &str = "\
CREATE TABLE IF NOT EXISTS entity (\
  id TEXT PRIMARY KEY,\
  name TEXT NOT NULL,\
  aliases TEXT NOT NULL DEFAULT '[]',\
  entity_type TEXT NOT NULL\
)";

pub const MEMORY_ENTITY_DDL: &str = "\
CREATE TABLE IF NOT EXISTS memory_entity (\
  memory_id TEXT NOT NULL,\
  entity_id TEXT NOT NULL,\
  PRIMARY KEY (memory_id, entity_id),\
  FOREIGN KEY (memory_id) REFERENCES memory_item(id) ON DELETE CASCADE,\
  FOREIGN KEY (entity_id) REFERENCES entity(id) ON DELETE CASCADE\
)";

// PRD §9.2 Kuzu 替代方案 (裁定 2026-08-26: Kuzu 无 Rust binding, 换 SQLite 递归 CTE)。
// relation 表存实体间关系边; graph_affinity 用 WITH RECURSIVE 做 N-hop 遍历。
pub const RELATION_DDL: &str = "\
CREATE TABLE IF NOT EXISTS relation (\
  src TEXT NOT NULL,\
  dst TEXT NOT NULL,\
  rel_type TEXT NOT NULL,\
  weight REAL NOT NULL DEFAULT 1.0,\
  rule_priority INTEGER NOT NULL DEFAULT 0,\
  first_seen INTEGER NOT NULL DEFAULT 0,\
  PRIMARY KEY (src, dst, rel_type)\
)";

pub const CONSOLIDATION_LOG_DDL: &str = "\
CREATE TABLE IF NOT EXISTS consolidation_log (\
  id INTEGER PRIMARY KEY AUTOINCREMENT,\
  started_at INTEGER NOT NULL,\
  elapsed_ms INTEGER NOT NULL,\
  dropped INTEGER NOT NULL,\
  promoted INTEGER NOT NULL,\
  merged INTEGER NOT NULL,\
  summarized INTEGER NOT NULL,\
  reextracted INTEGER NOT NULL,\
  reconciled INTEGER NOT NULL\
)";

pub const MERGE_LOG_DDL: &str = "\
CREATE TABLE IF NOT EXISTS merge_log (\
  id INTEGER PRIMARY KEY AUTOINCREMENT,\
  at INTEGER NOT NULL,\
  source_id TEXT NOT NULL,\
  target_id TEXT NOT NULL,\
  reason TEXT NOT NULL\
)";

pub const RECONCILE_REPORT_DDL: &str = "\
CREATE TABLE IF NOT EXISTS reconcile_report (\
  id INTEGER PRIMARY KEY AUTOINCREMENT,\
  at INTEGER NOT NULL,\
  memory_id TEXT NOT NULL,\
  stage TEXT NOT NULL,\
  error TEXT NOT NULL\
)";

pub const WOP_LOG_DDL: &str = "\
CREATE TABLE IF NOT EXISTS wop_log (\
  seq INTEGER PRIMARY KEY AUTOINCREMENT,\
  op TEXT NOT NULL,\
  payload TEXT NOT NULL,\
  at INTEGER NOT NULL\
)";

// P1-3: 持久化审计日志 — 核心路径 (commit/retrieve/consolidate/delete) 的 who/when/what。
// actor=调用方标识 (commit 用 session_id, 其余 "system"/RPC origin); action=动作;
// target_id=操作对象 id; detail=自由文本 (如 retrieve query 摘要)。
pub const AUDIT_LOG_DDL: &str = "\
CREATE TABLE IF NOT EXISTS audit_log (\
  id INTEGER PRIMARY KEY AUTOINCREMENT,\
  at INTEGER NOT NULL,\
  actor TEXT NOT NULL,\
  action TEXT NOT NULL,\
  target_id TEXT NOT NULL,\
  detail TEXT NOT NULL\
)";

pub const INDEX_INTERACTION: &str = "\
CREATE INDEX IF NOT EXISTS idx_memory_interaction ON memory_item(interaction_id)";
pub const INDEX_SESSION: &str = "\
CREATE INDEX IF NOT EXISTS idx_memory_session ON memory_item(session_id)";
pub const INDEX_TIER: &str = "\
CREATE INDEX IF NOT EXISTS idx_memory_tier ON memory_item(tier, tombstone)";
pub const INDEX_WOP_SEQ: &str = "\
CREATE INDEX IF NOT EXISTS idx_wop_at ON wop_log(at)";

// §1.15: memory_entity(entity_id) 索引 — audit_by_entities 的 IN(entity_id...) join 走此索引而非全表扫。
// PK (memory_id, entity_id) 只对 memory_id 前缀有效, entity_id 反查需单独索引。
pub const INDEX_MEMORY_ENTITY_EID: &str = "\
CREATE INDEX IF NOT EXISTS idx_memory_entity_eid ON memory_entity(entity_id)";

// §1.3: memory_item(vector_ref) 索引 — retrieve_context 按 KNN 命中的 vector_ref 集合
// 定向 SELECT ... WHERE vector_ref IN (...) 走此索引, 替代旧版 list_all 全表扫 + HashMap。
// 旧版每 retrieve 加载全部非 tombstone 行 clone 进 HashMap 仅为查 ~10 个 KNN 命中, 10k 记忆 ≈ 2MB/次。
pub const INDEX_MEMORY_VECTOR_REF: &str = "\
CREATE INDEX IF NOT EXISTS idx_memory_vector_ref ON memory_item(vector_ref)";

pub const ALL_DDL: &[&str] = &[
    MEMORY_ITEM_DDL,
    ENTITY_DDL,
    MEMORY_ENTITY_DDL,
    RELATION_DDL,
    CONSOLIDATION_LOG_DDL,
    MERGE_LOG_DDL,
    RECONCILE_REPORT_DDL,
    WOP_LOG_DDL,
    AUDIT_LOG_DDL,
    INDEX_INTERACTION,
    INDEX_SESSION,
    INDEX_TIER,
    INDEX_WOP_SEQ,
    INDEX_MEMORY_ENTITY_EID,
    INDEX_MEMORY_VECTOR_REF,
];

pub const ALL_PRAGMAS: &[&str] = &[PRAGMA_BUSY, PRAGMA_SYNC, PRAGMA_FK];

// §1.10: 当前 schema 版本。每次不兼容变更 (加列/改类型/删表) 递增并写 migration。
// v0: 初始 (M0-M6 累积表结构, 全 CREATE TABLE IF NOT EXISTS, 无 user_version)。
// v1: 加 idx_memory_entity_eid 索引 (§1.15 audit 反查); 旧库已由 IF NOT EXISTS 补建, 版本号记录此点。
// v2: 加 idx_memory_vector_ref 索引 (§1.3 retrieve 定向查询); 旧库由 v1→v2 迁移步补建。
pub const SCHEMA_VERSION: u32 = 2;

/// §1.10: 读旧库 user_version。新库/空 PRAGMA 返 0。
pub fn user_version(conn: &rusqlite::Connection) -> rusqlite::Result<u32> {
    let v: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    Ok(v as u32)
}

/// §1.10: 写 user_version。
pub fn set_user_version(conn: &rusqlite::Connection, version: u32) -> rusqlite::Result<()> {
    // PRAGMA user_version 不支持绑定参数, 拼字符串 (version 为内部常量, 非用户输入, 无注入面)。
    conn.execute_batch(&format!("PRAGMA user_version = {}", version))
}

/// §1.10: 从旧版本迁移到当前 SCHEMA_VERSION。
/// 旧版无 user_version (恒 0) → 跑全量 ALL_DDL (IF NOT EXISTS 幂等) + 设版本号。
/// 后续不兼容变更在此按 from..to 分支加 ALTER/数据回填, 每段独立可回溯。
pub fn migrate(conn: &rusqlite::Connection) -> rusqlite::Result<()> {
    let from = user_version(conn)?;
    if from >= SCHEMA_VERSION {
        return Ok(());
    }
    // v0 → v1: 表/索引全 IF NOT EXISTS 幂等补建 (含 §1.15 新索引)。
    for ddl in ALL_DDL {
        conn.execute_batch(ddl)?;
    }
    set_user_version(conn, SCHEMA_VERSION)?;
    tracing::info!(from = from, to = SCHEMA_VERSION, "schema migrated");
    Ok(())
}
