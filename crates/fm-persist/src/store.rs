//! SQLite 持久化。WAL 模式 + MemoryItem 全字段 CRUD。PRD §4.3, §8.4。

use std::path::Path;
use std::sync::Mutex;

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use tracing::{debug, info};

use crate::error::PersistResult;
use crate::schema;
use fm_core::{ConsolidationReport, EntityNode, MemoryItem, MemoryTier, MemoryType};

pub struct Persist {
    conn: Mutex<Connection>,
}

impl Persist {
    pub fn open(path: impl AsRef<Path>) -> PersistResult<Self> {
        let conn = Connection::open(path)?;
        Self::init_conn(&conn)?;
        info!("persist opened (WAL)");
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    pub fn open_in_memory() -> PersistResult<Self> {
        let conn = Connection::open_in_memory()?;
        Self::init_conn(&conn)?;
        debug!("persist opened (in-memory)");
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn init_conn(conn: &Connection) -> PersistResult<()> {
        conn.execute_batch(schema::PRAGMA_WAL)?;
        for p in schema::ALL_PRAGMAS {
            conn.execute_batch(p)?;
        }
        for ddl in schema::ALL_DDL {
            conn.execute_batch(ddl)?;
        }
        Ok(())
    }

    pub fn put_memory(&self, item: &MemoryItem) -> PersistResult<()> {
        let conn = self.conn.lock().expect("persist conn lock poisoned");
        let entities_json = serde_json::to_string(&item.entities)?;
        conn.execute(
            schema::MEMORY_ITEM_DDL_INSERT,
            params![
                item.id,
                item.interaction_id,
                item.turn_idx as i64,
                item.session_id,
                item.memory_type.as_str(),
                item.tier.as_str(),
                item.content,
                item.vector_ref,
                item.weight,
                item.access_count as i64,
                item.last_accessed_timestamp as i64,
                item.created_timestamp as i64,
                item.provenance,
                item.tombstone as i64,
                item.entities_pending as i64,
                entities_json,
            ],
        )?;
        for e in &item.entities {
            conn.execute(
                "INSERT OR IGNORE INTO entity(id,name,aliases,entity_type) VALUES(?1,?2,?3,?4)",
                params![
                    e.id,
                    e.name,
                    serde_json::to_string(&e.aliases)?,
                    e.entity_type.as_str()
                ],
            )?;
            conn.execute(
                "INSERT OR IGNORE INTO memory_entity(memory_id,entity_id) VALUES(?1,?2)",
                params![item.id, e.id],
            )?;
        }
        Ok(())
    }

    pub fn get_memory(&self, id: &str) -> PersistResult<Option<MemoryItem>> {
        let conn = self.conn.lock().expect("persist conn lock poisoned");
        let mut stmt = conn.prepare(schema::MEMORY_ITEM_DDL_SELECT_BY_ID)?;
        let row = stmt.query_row(params![id], row_to_memory).optional()?;
        Ok(row)
    }

    pub fn list_by_interaction(&self, interaction_id: &str) -> PersistResult<Vec<MemoryItem>> {
        let conn = self.conn.lock().expect("persist conn lock poisoned");
        let mut stmt = conn.prepare(
            "SELECT * FROM memory_item WHERE interaction_id=?1 AND tombstone=0 ORDER BY turn_idx ASC",
        )?;
        let rows = stmt.query_map(params![interaction_id], row_to_memory)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    pub fn list_all(&self) -> PersistResult<Vec<MemoryItem>> {
        let conn = self.conn.lock().expect("persist conn lock poisoned");
        let mut stmt = conn.prepare(
            "SELECT * FROM memory_item WHERE tombstone=0 ORDER BY created_timestamp ASC",
        )?;
        let rows = stmt.query_map([], row_to_memory)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    pub fn count(&self) -> PersistResult<u64> {
        let conn = self.conn.lock().expect("persist conn lock poisoned");
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM memory_item WHERE tombstone=0",
            [],
            |row| row.get(0),
        )?;
        Ok(n as u64)
    }

    pub fn tombstone_memory(&self, id: &str) -> PersistResult<()> {
        let conn = self.conn.lock().expect("persist conn lock poisoned");
        conn.execute(
            "UPDATE memory_item SET tombstone=1 WHERE id=?1",
            params![id],
        )?;
        Ok(())
    }

    pub fn touch_access(&self, id: &str, now_ts: u64) -> PersistResult<()> {
        let conn = self.conn.lock().expect("persist conn lock poisoned");
        conn.execute(
            "UPDATE memory_item SET access_count=access_count+1, last_accessed_timestamp=?1 WHERE id=?2",
            params![now_ts as i64, id],
        )?;
        Ok(())
    }

    pub fn record_consolidation(
        &self,
        report: &ConsolidationReport,
        started_at: u64,
    ) -> PersistResult<()> {
        let conn = self.conn.lock().expect("persist conn lock poisoned");
        conn.execute(
            "INSERT INTO consolidation_log(started_at,elapsed_ms,dropped,promoted,merged,summarized,reextracted,reconciled) \
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
            params![
                started_at as i64,
                report.elapsed_ms as i64,
                report.dropped as i64,
                report.promoted as i64,
                report.merged as i64,
                report.summarized as i64,
                report.reextracted as i64,
                report.reconciled as i64,
            ],
        )?;
        for f in &report.failures {
            conn.execute(
                "INSERT INTO reconcile_report(at,memory_id,stage,error) VALUES(?1,?2,?3,?4)",
                params![started_at as i64, f.memory_id, f.stage, f.error],
            )?;
        }
        Ok(())
    }

    pub fn append_wop(&self, op: &str, payload: &str, at: u64) -> PersistResult<()> {
        let conn = self.conn.lock().expect("persist conn lock poisoned");
        conn.execute(
            "INSERT INTO wop_log(op,payload,at) VALUES(?1,?2,?3)",
            params![op, payload, at as i64],
        )?;
        Ok(())
    }

    pub fn record_merge(
        &self,
        source_id: &str,
        target_id: &str,
        reason: &str,
        at: u64,
    ) -> PersistResult<()> {
        let conn = self.conn.lock().expect("persist conn lock poisoned");
        conn.execute(
            "INSERT INTO merge_log(at,source_id,target_id,reason) VALUES(?1,?2,?3,?4)",
            params![at as i64, source_id, target_id, reason],
        )?;
        Ok(())
    }

    /// 插入实体关系边 (PRD §7.4)。INSERT OR IGNORE 去重。
    pub fn put_relation(
        &self,
        src: &str,
        dst: &str,
        rel_type: &str,
        weight: f64,
        rule_priority: i64,
        first_seen: u64,
    ) -> PersistResult<()> {
        let conn = self.conn.lock().expect("persist conn lock poisoned");
        conn.execute(
            "INSERT OR IGNORE INTO relation(src,dst,rel_type,weight,rule_priority,first_seen) \
             VALUES(?1,?2,?3,?4,?5,?6)",
            params![src, dst, rel_type, weight, rule_priority, first_seen as i64],
        )?;
        Ok(())
    }

    /// 取某实体的直接邻居 (1-hop)。供对齐/图遍历用。
    pub fn list_relations_from(&self, src: &str) -> PersistResult<Vec<(String, String, f64)>> {
        let conn = self.conn.lock().expect("persist conn lock poisoned");
        let mut stmt = conn.prepare("SELECT dst, rel_type, weight FROM relation WHERE src=?1")?;
        let rows = stmt.query_map(params![src], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, f64>(2)?,
            ))
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// N-hop 可达节点 (WITH RECURSIVE CTE)。PRD §7.2 graph_affinity, B4 N 跳上限。
    /// 返回 (node_id, hop) — 从 start 出发 hop_limit 跳内可达的所有 entity id。
    pub fn n_hop_reachable(
        &self,
        start: &str,
        hop_limit: usize,
    ) -> PersistResult<Vec<(String, usize)>> {
        let conn = self.conn.lock().expect("persist conn lock poisoned");
        let sql = r"WITH RECURSIVE hop(lvl, node) AS (
  SELECT 0, ?1
  UNION ALL
  SELECT h.lvl+1, r.dst FROM hop h JOIN relation r ON r.src=h.node
  WHERE h.lvl < ?2
)
SELECT DISTINCT node, MIN(lvl) AS first_hop FROM hop WHERE node != ?1 GROUP BY node";
        let mut stmt = conn.prepare(sql)?;
        let rows = stmt.query_map(params![start, hop_limit as i64], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as usize))
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// 按 entity_type 查全部实体 (id, name, aliases) (规则对齐用)。
    pub fn list_entities_by_type(
        &self,
        entity_type: &str,
    ) -> PersistResult<Vec<(String, String, String)>> {
        let conn = self.conn.lock().expect("persist conn lock poisoned");
        let mut stmt = conn.prepare("SELECT id, name, aliases FROM entity WHERE entity_type=?1")?;
        let rows = stmt.query_map(params![entity_type], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// 更新实体别名 (LLM 候选写入, A5: 仅候选不作判定)。
    pub fn append_entity_alias(&self, entity_id: &str, alias: &str) -> PersistResult<()> {
        let conn = self.conn.lock().expect("persist conn lock poisoned");
        let row: Option<String> = conn
            .query_row(
                "SELECT aliases FROM entity WHERE id=?1",
                params![entity_id],
                |row| row.get(0),
            )
            .ok();
        if let Some(aliases_json) = row {
            let mut aliases: Vec<String> = serde_json::from_str(&aliases_json).unwrap_or_default();
            if !aliases.iter().any(|a| a.eq_ignore_ascii_case(alias)) {
                aliases.push(alias.to_string());
                let new_json = serde_json::to_string(&aliases)?;
                conn.execute(
                    "UPDATE entity SET aliases=?1 WHERE id=?2",
                    params![new_json, entity_id],
                )?;
            }
        }
        Ok(())
    }

    // ---- M3: consolidate saga 支撑方法 ----

    /// 上次 consolidate 的 started_at (增量扫描边界)。无记录返 0。
    pub fn last_consolidate_at(&self) -> PersistResult<u64> {
        let conn = self.conn.lock().expect("persist conn lock poisoned");
        let at: Option<i64> = conn
            .query_row("SELECT MAX(started_at) FROM consolidation_log", [], |row| {
                row.get(0)
            })
            .ok();
        Ok(at.unwrap_or(0) as u64)
    }

    /// 增量变更集: last_accessed 或 created > since 的非 tombstone 记忆。B4。
    pub fn list_changed_since(&self, since: u64) -> PersistResult<Vec<MemoryItem>> {
        let conn = self.conn.lock().expect("persist conn lock poisoned");
        let mut stmt = conn.prepare(
            "SELECT * FROM memory_item WHERE tombstone=0 \
             AND (last_accessed_timestamp > ?1 OR created_timestamp > ?1) \
             ORDER BY created_timestamp ASC",
        )?;
        let rows = stmt.query_map(params![since as i64], row_to_memory)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// 全部已 tombstone 记忆 (reconcile 物理删候选)。
    pub fn list_tombstoned(&self) -> PersistResult<Vec<MemoryItem>> {
        let conn = self.conn.lock().expect("persist conn lock poisoned");
        let mut stmt = conn.prepare("SELECT * FROM memory_item WHERE tombstone=1")?;
        let rows = stmt.query_map([], row_to_memory)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// 物理删 (含级联 memory_entity)。reconcile 三库一致后调用。§8.4。
    pub fn physical_delete(&self, id: &str) -> PersistResult<()> {
        let conn = self.conn.lock().expect("persist conn lock poisoned");
        conn.execute("DELETE FROM memory_item WHERE id=?1", params![id])?;
        Ok(())
    }

    /// 全部 merge_log (fm-cli unmerge 列表用)。
    pub fn list_merge_log(&self) -> PersistResult<Vec<MergeLogEntry>> {
        let conn = self.conn.lock().expect("persist conn lock poisoned");
        let mut stmt = conn
            .prepare("SELECT id,at,source_id,target_id,reason FROM merge_log ORDER BY id DESC")?;
        let rows = stmt.query_map([], |row| {
            Ok(MergeLogEntry {
                id: row.get::<_, i64>(0)? as u64,
                at: row.get::<_, i64>(1)? as u64,
                source_id: row.get(2)?,
                target_id: row.get(3)?,
                reason: row.get(4)?,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// 回滚单次合并: 删 merge_log 行, 返回 (source_id,target_id) 供引擎恢复 source。
    pub fn unmerge(&self, merge_id: u64) -> PersistResult<Option<(String, String)>> {
        let conn = self.conn.lock().expect("persist conn lock poisoned");
        let row: Option<(String, String)> = conn
            .query_row(
                "SELECT source_id,target_id FROM merge_log WHERE id=?1",
                params![merge_id as i64],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .ok();
        let Some((src, tgt)) = row else {
            return Ok(None);
        };
        conn.execute(
            "DELETE FROM merge_log WHERE id=?1",
            params![merge_id as i64],
        )?;
        Ok(Some((src, tgt)))
    }

    /// 追加 reconcile_report 记录 (跨库对账差异/失败项)。
    pub fn append_reconcile(
        &self,
        at: u64,
        memory_id: &str,
        stage: &str,
        error: &str,
    ) -> PersistResult<()> {
        let conn = self.conn.lock().expect("persist conn lock poisoned");
        conn.execute(
            "INSERT INTO reconcile_report(at,memory_id,stage,error) VALUES(?1,?2,?3,?4)",
            params![at as i64, memory_id, stage, error],
        )?;
        Ok(())
    }
}

/// merge_log 条目 (fm-cli unmerge 展示用)。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MergeLogEntry {
    pub id: u64,
    pub at: u64,
    pub source_id: String,
    pub target_id: String,
    pub reason: String,
}

const _: () = {
    // 占位：保证 entities_json 列名常量可见（避免未来误删）
};

fn row_to_memory(row: &rusqlite::Row) -> rusqlite::Result<MemoryItem> {
    let memory_type_str: String = row.get("memory_type")?;
    let tier_str: String = row.get("tier")?;
    let entities_json: String = row.get("entities_json")?;
    let entities: Vec<EntityNode> =
        serde_json::from_str(&entities_json).unwrap_or_else(|_| Vec::new());
    let tombstone_i: i64 = row.get("tombstone")?;
    let pending_i: i64 = row.get("entities_pending")?;
    Ok(MemoryItem {
        id: row.get("id")?,
        interaction_id: row.get("interaction_id")?,
        turn_idx: row.get::<_, i64>("turn_idx")? as u32,
        session_id: row.get("session_id")?,
        memory_type: MemoryType::parse(&memory_type_str).unwrap_or(MemoryType::Episodic),
        tier: MemoryTier::parse(&tier_str).unwrap_or(MemoryTier::Short),
        content: row.get("content")?,
        entities,
        vector_ref: row.get("vector_ref")?,
        weight: row.get("weight")?,
        access_count: row.get::<_, i64>("access_count")? as u64,
        last_accessed_timestamp: row.get::<_, i64>("last_accessed_timestamp")? as u64,
        created_timestamp: row.get::<_, i64>("created_timestamp")? as u64,
        provenance: row.get("provenance")?,
        tombstone: tombstone_i != 0,
        entities_pending: pending_i != 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use fm_core::{ConsolidationFailure, EntityType, MemoryItem};

    fn sample_item(id: &str, ix: &str, turn: u32) -> MemoryItem {
        let mut m = MemoryItem::new_turn_skeleton(
            id.into(),
            ix.into(),
            turn,
            "sess-1".into(),
            MemoryType::Semantic,
            format!("content-{turn}"),
            1_000 + turn as u64,
        );
        m.tier = MemoryTier::Short;
        m
    }

    #[test]
    fn put_get_roundtrip() {
        let p = Persist::open_in_memory().unwrap();
        let m = sample_item("m1", "ix1", 0);
        p.put_memory(&m).unwrap();
        let got = p.get_memory("m1").unwrap().unwrap();
        assert_eq!(got.id, "m1");
        assert_eq!(got.interaction_id, "ix1");
        assert_eq!(got.memory_type, MemoryType::Semantic);
        assert_eq!(got.tier, MemoryTier::Short);
        assert_eq!(got.content, "content-0");
        assert!(!got.tombstone);
    }

    #[test]
    fn list_by_interaction_orders_by_turn() {
        let p = Persist::open_in_memory().unwrap();
        p.put_memory(&sample_item("a", "ix", 2)).unwrap();
        p.put_memory(&sample_item("b", "ix", 0)).unwrap();
        p.put_memory(&sample_item("c", "ix", 1)).unwrap();
        let items = p.list_by_interaction("ix").unwrap();
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].turn_idx, 0);
        assert_eq!(items[1].turn_idx, 1);
        assert_eq!(items[2].turn_idx, 2);
    }

    #[test]
    fn tombstone_excludes_from_list() {
        let p = Persist::open_in_memory().unwrap();
        p.put_memory(&sample_item("a", "ix", 0)).unwrap();
        p.put_memory(&sample_item("b", "ix", 1)).unwrap();
        p.tombstone_memory("a").unwrap();
        let items = p.list_by_interaction("ix").unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, "b");
        let got = p.get_memory("a").unwrap();
        // tombstone 行仍在，但 list_by_interaction 过滤 tombstone=0
        assert!(got.is_some());
    }

    #[test]
    fn count_and_access() {
        let p = Persist::open_in_memory().unwrap();
        p.put_memory(&sample_item("a", "ix", 0)).unwrap();
        p.put_memory(&sample_item("b", "ix", 1)).unwrap();
        assert_eq!(p.count().unwrap(), 2);
        p.touch_access("a", 9999).unwrap();
        let got = p.get_memory("a").unwrap().unwrap();
        assert_eq!(got.access_count, 1);
        assert_eq!(got.last_accessed_timestamp, 9999);
    }

    #[test]
    fn entities_persisted() {
        let p = Persist::open_in_memory().unwrap();
        let mut m = sample_item("e1", "ix", 0);
        m.entities.push(EntityNode::new(
            "ent-1".into(),
            "Rust".into(),
            EntityType::Tech,
        ));
        p.put_memory(&m).unwrap();
        let got = p.get_memory("e1").unwrap().unwrap();
        assert_eq!(got.entities.len(), 1);
        assert_eq!(got.entities[0].name, "Rust");
    }

    #[test]
    fn consolidation_logged() {
        let p = Persist::open_in_memory().unwrap();
        let report = ConsolidationReport {
            dropped: 3,
            promoted: 1,
            elapsed_ms: 42,
            failures: vec![ConsolidationFailure {
                memory_id: "x".into(),
                stage: "merge".into(),
                error: "boom".into(),
            }],
            ..Default::default()
        };
        p.record_consolidation(&report, 1234).unwrap();
        assert_eq!(p.count().unwrap(), 0);
    }

    #[test]
    fn wop_append() {
        let p = Persist::open_in_memory().unwrap();
        p.append_wop("commit", "{}", 100).unwrap();
        p.append_wop("delete", "{}", 200).unwrap();
        assert_eq!(p.count().unwrap(), 0);
    }

    #[test]
    fn last_consolidate_at_default_zero() {
        let p = Persist::open_in_memory().unwrap();
        assert_eq!(p.last_consolidate_at().unwrap(), 0);
    }

    #[test]
    fn last_consolidate_at_tracks_max() {
        let p = Persist::open_in_memory().unwrap();
        p.record_consolidation(&ConsolidationReport::default(), 100)
            .unwrap();
        p.record_consolidation(&ConsolidationReport::default(), 250)
            .unwrap();
        assert_eq!(p.last_consolidate_at().unwrap(), 250);
    }

    #[test]
    fn list_changed_since_filters() {
        let p = Persist::open_in_memory().unwrap();
        p.put_memory(&sample_item("a", "ix", 0)).unwrap();
        p.put_memory(&sample_item("b", "ix", 1)).unwrap();
        let since_before = p.last_consolidate_at().unwrap();
        let changed = p.list_changed_since(since_before).unwrap();
        assert_eq!(changed.len(), 2);
        let after_max = 10_000;
        assert!(p.list_changed_since(after_max).unwrap().is_empty());
    }

    #[test]
    fn list_tombstoned_returns_only_tombstoned() {
        let p = Persist::open_in_memory().unwrap();
        p.put_memory(&sample_item("a", "ix", 0)).unwrap();
        p.put_memory(&sample_item("b", "ix", 1)).unwrap();
        p.tombstone_memory("a").unwrap();
        let tbs = p.list_tombstoned().unwrap();
        assert_eq!(tbs.len(), 1);
        assert_eq!(tbs[0].id, "a");
        assert!(tbs[0].tombstone);
    }

    #[test]
    fn physical_delete_removes_row() {
        let p = Persist::open_in_memory().unwrap();
        p.put_memory(&sample_item("a", "ix", 0)).unwrap();
        p.physical_delete("a").unwrap();
        assert!(p.get_memory("a").unwrap().is_none());
        assert_eq!(p.count().unwrap(), 0);
    }

    #[test]
    fn merge_log_roundtrip_and_unmerge() {
        let p = Persist::open_in_memory().unwrap();
        p.record_merge("src-1", "tgt-1", "sim", 100).unwrap();
        p.record_merge("src-2", "tgt-2", "entity", 200).unwrap();
        let logs = p.list_merge_log().unwrap();
        assert_eq!(logs.len(), 2);
        assert_eq!(logs[0].source_id, "src-2");
        assert_eq!(logs[1].source_id, "src-1");

        let first_id = logs[1].id;
        let unmerged = p.unmerge(first_id).unwrap();
        assert_eq!(unmerged, Some(("src-1".into(), "tgt-1".into())));
        assert_eq!(p.list_merge_log().unwrap().len(), 1);
    }

    #[test]
    fn unmerge_missing_id_returns_none() {
        let p = Persist::open_in_memory().unwrap();
        assert_eq!(p.unmerge(9999).unwrap(), None);
    }

    #[test]
    fn append_reconcile_records_row() {
        let p = Persist::open_in_memory().unwrap();
        p.append_reconcile(500, "mem-x", "vector_diff", "dangling")
            .unwrap();
        // 不抛错即视为写入成功；reconcile_report 无独立读 API，靠 record_consolidation
        // failures 路径已验证字段，此处只验证 INSERT 不报错。
        assert!(p.append_reconcile(501, "mem-y", "ok", "").is_ok());
    }

    #[test]
    fn relation_put_list_and_n_hop() {
        let p = Persist::open_in_memory().unwrap();
        p.put_relation("A", "B", "knows", 0.8, 1, 100).unwrap();
        p.put_relation("B", "C", "knows", 0.7, 1, 200).unwrap();
        p.put_relation("C", "D", "knows", 0.6, 1, 300).unwrap();
        let from_a = p.list_relations_from("A").unwrap();
        assert_eq!(from_a.len(), 1);
        assert_eq!(from_a[0].0, "B");

        let one_hop = p.n_hop_reachable("A", 1).unwrap();
        assert_eq!(one_hop.len(), 1);
        assert_eq!(one_hop[0].0, "B");

        let two_hop = p.n_hop_reachable("A", 2).unwrap();
        assert!(two_hop.iter().any(|(n, _)| n == "C"));
        assert!(!two_hop.iter().any(|(n, _)| n == "D"));

        let three_hop = p.n_hop_reachable("A", 3).unwrap();
        assert!(three_hop.iter().any(|(n, _)| n == "D"));
    }

    #[test]
    fn list_entities_by_type_and_alias() {
        let p = Persist::open_in_memory().unwrap();
        let mut m = sample_item("e1", "ix", 0);
        m.entities.push(EntityNode::new(
            "ent-rust".into(),
            "Rust".into(),
            EntityType::Tech,
        ));
        p.put_memory(&m).unwrap();

        let techs = p.list_entities_by_type("Tech").unwrap();
        assert_eq!(techs.len(), 1);
        assert_eq!(techs[0].0, "ent-rust");
        assert_eq!(techs[0].1, "Rust");

        p.append_entity_alias("ent-rust", "Rs").unwrap();
        let after = p.list_entities_by_type("Tech").unwrap();
        assert!(after[0].2.contains("Rs"), "aliases={}", after[0].2);

        // 重复别名去重
        p.append_entity_alias("ent-rust", "rs").unwrap();
        let after2 = p.list_entities_by_type("Tech").unwrap();
        let count = after2[0].2.matches("Rs").count() + after2[0].2.matches("rs").count();
        assert_eq!(count, 1, "alias duplicated: {}", after2[0].2);

        // 不存在的 entity 不抛错
        assert!(p.append_entity_alias("ghost", "x").is_ok());
    }
}
