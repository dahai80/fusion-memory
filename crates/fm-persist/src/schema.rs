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

pub const INDEX_INTERACTION: &str = "\
CREATE INDEX IF NOT EXISTS idx_memory_interaction ON memory_item(interaction_id)";
pub const INDEX_SESSION: &str = "\
CREATE INDEX IF NOT EXISTS idx_memory_session ON memory_item(session_id)";
pub const INDEX_TIER: &str = "\
CREATE INDEX IF NOT EXISTS idx_memory_tier ON memory_item(tier, tombstone)";
pub const INDEX_WOP_SEQ: &str = "\
CREATE INDEX IF NOT EXISTS idx_wop_at ON wop_log(at)";

pub const ALL_DDL: &[&str] = &[
    MEMORY_ITEM_DDL,
    ENTITY_DDL,
    MEMORY_ENTITY_DDL,
    RELATION_DDL,
    CONSOLIDATION_LOG_DDL,
    MERGE_LOG_DDL,
    RECONCILE_REPORT_DDL,
    WOP_LOG_DDL,
    INDEX_INTERACTION,
    INDEX_SESSION,
    INDEX_TIER,
    INDEX_WOP_SEQ,
];

pub const ALL_PRAGMAS: &[&str] = &[PRAGMA_BUSY, PRAGMA_SYNC, PRAGMA_FK];
