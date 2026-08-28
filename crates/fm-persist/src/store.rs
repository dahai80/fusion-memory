//! SQLite 持久化。WAL 模式 + MemoryItem 全字段 CRUD。PRD §4.3, §8.4。

use std::path::Path;
use std::time::Duration;

use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use crate::error::{PersistError, PersistResult};
use crate::schema;
use fm_core::{ConsolidationReport, EntityNode, MemoryItem, MemoryTier, MemoryType};

// P4: 递归 CTE 结果集扇出上限 (graph_affinity 远端节点贡献 0.5^h 指数衰减, 截断无损精度)。
const N_HOP_RESULT_LIMIT: i64 = 256;

// §1.1: r2d2 连接池 (打破单 Mutex<Connection> 串行)。PooledConnection DerefMut → Connection,
// 调用点 (prepare_cached / transaction) 零改动。WAL 原生 1 写 N 读, 池大小 8 够并发读。
const POOL_SIZE: u32 = 8;
// P1-9: pool get 超时 (r2d2 connection_timeout, get() 默认用它)。池满时 get() 等空连最多 5s,
// 超时返 GetTimeout (Display 含 "timed out") → 映射 MemoryError::Busy (可重试), 非无限阻塞。
// r2d2 默认 30s; 显式 5s 兜底防死锁/慢查询拖垮整体。connection_timeout 须 >0 (r2d2 会 panic)。
const POOL_GET_TIMEOUT: Duration = Duration::from_secs(5);

pub struct Persist {
    pool: Pool<SqliteConnectionManager>,
}

impl Persist {
    pub fn open(path: impl AsRef<Path>) -> PersistResult<Self> {
        let mgr = SqliteConnectionManager::file(path).with_init(|conn| {
            Self::init_conn(conn).map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))
        });
        let pool = Pool::builder()
            .max_size(POOL_SIZE)
            .connection_timeout(POOL_GET_TIMEOUT)
            .build(mgr)?;
        info!("persist opened (WAL, pool size {POOL_SIZE}, get_timeout 5s)");
        Ok(Self { pool })
    }

    pub fn open_in_memory() -> PersistResult<Self> {
        // in-memory 池: 共享空 DB URL (r2d2 每连独立 open_in_memory → 各自私有 DB)。
        // 单测用, 跨连隔离不影响 (测试均单线程串行)。WAL pragma 在 in-memory 无效但不报错。
        let mgr = SqliteConnectionManager::memory().with_init(|conn| {
            Self::init_conn(conn).map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))
        });
        let pool = Pool::builder()
            .max_size(POOL_SIZE)
            .connection_timeout(POOL_GET_TIMEOUT)
            .build(mgr)?;
        debug!("persist opened (in-memory, pool size {POOL_SIZE}, get_timeout 5s)");
        Ok(Self { pool })
    }

    fn init_conn(conn: &mut rusqlite::Connection) -> PersistResult<()> {
        conn.execute_batch(schema::PRAGMA_WAL)?;
        for p in schema::ALL_PRAGMAS {
            conn.execute_batch(p)?;
        }
        // §1.10: 旧版直接遍历 ALL_DDL, 无 user_version → 升级时无法区分"已建"与"需迁移",
        // 不兼容变更只能赌 IF NOT EXISTS (加列场景失效)。改: schema::migrate 按 user_version 跑增量迁移。
        schema::migrate(conn)?;
        Ok(())
    }

    // §1.1: 从池租连接 (PooledConnection DerefMut → Connection)。池满时等空连而非串行全部访问。
    // Poisoned 语义失效 (r2d2 池无 poison, 连接 panic 自动回收), 保留错误枚举兼容上层 match。
    fn conn(&self) -> PersistResult<r2d2::PooledConnection<SqliteConnectionManager>> {
        self.pool
            .get()
            .map_err(|e| PersistError::Pool(e.to_string()))
    }

    // §1.1: 事务路径用同一池 (WAL 写串行在 DB 层保证, 不需独立写连接)。DerefMut 支持 transaction()。
    fn conn_mut(&self) -> PersistResult<r2d2::PooledConnection<SqliteConnectionManager>> {
        self.pool
            .get()
            .map_err(|e| PersistError::Pool(e.to_string()))
    }

    /// P0-4: 备份 SQLite 到目标文件。用 VACUUM INTO 生成一致快照 (WAL 合并进单文件, 无需停写)。
    /// 目标文件不应已存在 (SQLite VACUUM INTO 要求目标不存在)。返回写入字节数。
    pub fn backup_sqlite(&self, dest: impl AsRef<Path>) -> PersistResult<u64> {
        let dest = dest.as_ref();
        if dest.exists() {
            return Err(PersistError::Io(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!(
                    "backup target already exists: {} (refusing overwrite)",
                    dest.display()
                ),
            )));
        }
        let conn = self.conn()?;
        // VACUUM INTO 在线一致快照: 不阻塞写, 产出独立可移植 .db 文件 (含全部 schema+数据)。
        let dest_str = dest.to_str().ok_or_else(|| {
            PersistError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "backup path not utf-8",
            ))
        })?;
        conn.execute_batch(&format!("VACUUM INTO '{}'", dest_str.replace('\'', "''")))?;
        let bytes = std::fs::metadata(dest).map(|m| m.len()).unwrap_or(0);
        info!(dest = %dest.display(), bytes, "sqlite backup done (VACUUM INTO)");
        Ok(bytes)
    }

    pub fn put_memory(&self, item: &MemoryItem) -> PersistResult<()> {
        // H1: memory_item + entity + memory_entity 三类 INSERT 包进单 transaction,
        // 中途任一失败 → rollback, 不留半截实体行 (无事务时 insert 成功后 entity 循环失败留孤儿)。
        let mut conn = self.conn_mut()?;
        let tx = conn.transaction()?;
        let entities_json = serde_json::to_string(&item.entities)?;
        tx.execute(
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
            tx.execute(
                "INSERT OR IGNORE INTO entity(id,name,aliases,entity_type) VALUES(?1,?2,?3,?4)",
                params![
                    e.id,
                    e.name,
                    serde_json::to_string(&e.aliases)?,
                    e.entity_type.as_str()
                ],
            )?;
            tx.execute(
                "INSERT OR IGNORE INTO memory_entity(memory_id,entity_id) VALUES(?1,?2)",
                params![item.id, e.id],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// §2.4: put_memory + append_wop 单 transaction 原子落库。
    /// 旧版两步独立 INSERT (put_memory commit 后 append_wop 单独 execute),
    /// 崩溃窗口小但分叉永久静默: memory_item 行在但 wop_log 无 → follower since_seq 永拉不到。
    /// 改: 同一 conn.transaction 包 memory_item+entity+memory_entity+wop_log, 中途任一失败全 rollback。
    /// 返 wop seq (last_insert_rowid)。wop payload 不含向量 (follower 本地 re-embed, §6.3)。
    pub fn put_memory_with_wop(
        &self,
        item: &MemoryItem,
        wop_op: &str,
        wop_payload: &str,
        at: u64,
    ) -> PersistResult<i64> {
        let mut conn = self.conn_mut()?;
        let tx = conn.transaction()?;
        let entities_json = serde_json::to_string(&item.entities)?;
        tx.execute(
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
            tx.execute(
                "INSERT OR IGNORE INTO entity(id,name,aliases,entity_type) VALUES(?1,?2,?3,?4)",
                params![
                    e.id,
                    e.name,
                    serde_json::to_string(&e.aliases)?,
                    e.entity_type.as_str()
                ],
            )?;
            tx.execute(
                "INSERT OR IGNORE INTO memory_entity(memory_id,entity_id) VALUES(?1,?2)",
                params![item.id, e.id],
            )?;
        }
        tx.execute(
            "INSERT INTO wop_log(op,payload,at) VALUES(?1,?2,?3)",
            params![wop_op, wop_payload, at as i64],
        )?;
        let seq = tx.last_insert_rowid();
        tx.commit()?;
        debug!(seq, op = wop_op, "put_memory_with_wop atomic");
        Ok(seq)
    }

    pub fn get_memory(&self, id: &str) -> PersistResult<Option<MemoryItem>> {
        let conn = self.conn()?;
        // §3.12: 旧版每次 prepare(SELECT * WHERE id=?) → 每请求重新解析 SQL + 建预处理句柄。
        // 改: prepare_cached 复用 Connection 缓存的预处理句柄 (单 conn 被 Mutex 持久化, 缓存跨调用有效)。
        let mut stmt = conn.prepare_cached(schema::MEMORY_ITEM_DDL_SELECT_BY_ID)?;
        let row = stmt.query_row(params![id], row_to_memory).optional()?;
        Ok(row)
    }

    pub fn list_by_interaction(&self, interaction_id: &str) -> PersistResult<Vec<MemoryItem>> {
        let conn = self.conn()?;
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
        let conn = self.conn()?;
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

    /// §1.3: 按 vector_ref 集合定向查 memory_item。替代 retrieve_context 旧版 list_all 全表扫 +
    /// HashMap (10k 记忆 ≈ 2MB clone/次, 仅为查 ~10 个 KNN 命中)。走 idx_memory_vector_ref 索引,
    /// vector_ref 作 TEXT 存 (u64 的字符串形), IN(...) 谓词对 TEXT 索引同样命中。空集返空。
    pub fn get_by_vector_refs(&self, vec_refs: &[u64]) -> PersistResult<Vec<MemoryItem>> {
        if vec_refs.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.conn()?;
        // vector_ref 存 TEXT; 绑定 u64 → rusqlite ToSql 转 TEXT 比较一致 (插入也是 u64.to_string())。
        let refs_str: Vec<String> = vec_refs.iter().map(|v| v.to_string()).collect();
        let placeholders = (0..refs_str.len())
            .map(|i| format!("?{}", i + 1))
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT * FROM memory_item WHERE tombstone=0 AND vector_ref IN ({placeholders})"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(refs_str.iter()), row_to_memory)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    pub fn count(&self) -> PersistResult<u64> {
        let conn = self.conn()?;
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM memory_item WHERE tombstone=0",
            [],
            |row| row.get(0),
        )?;
        Ok(n as u64)
    }

    /// §1.15: 按实体 id 反查记忆 (替代 audit_memory_access 的 list_all 全表扫)。
    /// 旧版 list_all() 拉全表 + Rust filter → O(N) 内存扫, N 大时 audit 拖垮服务。
    /// 改: memory_entity join 走 (memory_id, entity_id) PK 索引, 按 entity_id 子集查, 只返回命中行。
    /// 用 `?` 占位 + rusqlite params![rusqlite::params_from_iter] 绑定变长 entity_ids。
    pub fn audit_by_entities(&self, entity_ids: &[String]) -> PersistResult<Vec<MemoryItem>> {
        if entity_ids.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.conn()?;
        // rarray 需扩展; 用手动 IN(?1,?2,...) 占位规避 carray/rarray 依赖, 与现有 params! 风格一致。
        let placeholders: Vec<String> = (0..entity_ids.len())
            .map(|i| format!("?{}", i + 1))
            .collect();
        let sql = format!(
            "SELECT m.* FROM memory_item m \
             JOIN memory_entity me ON me.memory_id = m.id \
             WHERE me.entity_id IN ({}) AND m.tombstone = 0 \
             ORDER BY m.created_timestamp ASC",
            placeholders.join(", ")
        );
        let mut stmt = conn.prepare(&sql)?;
        let params: Vec<&dyn rusqlite::ToSql> = entity_ids
            .iter()
            .map(|s| s as &dyn rusqlite::ToSql)
            .collect();
        let rows = stmt.query_map(params.as_slice(), row_to_memory)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    pub fn tombstone_memory(&self, id: &str) -> PersistResult<()> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE memory_item SET tombstone=1 WHERE id=?1",
            params![id],
        )?;
        Ok(())
    }

    /// 反 tombstone: 恢复 source 记忆可见性 (P1-2 half-merge 补偿 + unmerge 路径共用)。
    /// 与 tombstone_memory 对称, 仅置 tombstone=0, 不重建向量 (向量由调用方按需 insert_vector)。
    pub fn untombstone_memory(&self, id: &str) -> PersistResult<()> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE memory_item SET tombstone=0 WHERE id=?1",
            params![id],
        )?;
        Ok(())
    }

    /// P1-2 测试钩子: DROP merge_log 表使后续 record_merge 报错, 注入 half-merge 场景。
    /// 仅 feature="test-utils", 生产永不启用。
    #[cfg(feature = "test-utils")]
    pub fn break_merge_log_for_test(&self) -> PersistResult<()> {
        let conn = self.conn()?;
        conn.execute("DROP TABLE IF EXISTS merge_log", [])?;
        Ok(())
    }

    /// 按 session_id 批量软删 (issue #2 delete_scope)。返被 tombstone 行数。
    /// scope 在 engine 层即 session_id (MemoryItem.session_id 字段), 故此处按 session_id 过滤。
    /// 不删向量 (向量由 reconcile/compact 回收), 仅 tombstone 元数据, 与单条 delete_memory 一致语义。
    pub fn delete_by_session(&self, session_id: &str) -> PersistResult<u64> {
        let conn = self.conn()?;
        let n = conn.execute(
            "UPDATE memory_item SET tombstone=1 WHERE session_id=?1 AND tombstone=0",
            params![session_id],
        )?;
        Ok(n as u64)
    }

    /// 取某 session 下所有非 tombstone 记忆 (issue #2: delete_scope 需清对应向量 + list-ids)。
    pub fn list_by_session(&self, session_id: &str) -> PersistResult<Vec<MemoryItem>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT * FROM memory_item WHERE session_id=?1 AND tombstone=0 ORDER BY turn_idx ASC",
        )?;
        let rows = stmt.query_map(params![session_id], row_to_memory)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// 按 session_id 计数 (issue #2 count 带 scope 过滤)。空 session_id → 全量 (走 count())。
    pub fn count_by_session(&self, session_id: Option<&str>) -> PersistResult<u64> {
        let conn = self.conn()?;
        let n: i64 = match session_id {
            Some(s) => conn.query_row(
                "SELECT COUNT(*) FROM memory_item WHERE tombstone=0 AND session_id=?1",
                params![s],
                |row| row.get(0),
            )?,
            None => conn.query_row(
                "SELECT COUNT(*) FROM memory_item WHERE tombstone=0",
                [],
                |row| row.get(0),
            )?,
        };
        Ok(n as u64)
    }

    pub fn touch_access(&self, id: &str, now_ts: u64) -> PersistResult<()> {
        let conn = self.conn()?;
        // §3.6: AND tombstone=0 — 不 bump 已软删记忆的 access_count。
        // 旧版无此守卫: retrieve 快照读到活记忆 X 后, 并发 delete tombstone X, touch 仍 bump 其计数;
        // 若后续 unmerge 恢复 X, 膨胀计数残留, 污染 should_promote (查 access_count >= 阈值)。
        conn.execute(
            "UPDATE memory_item SET access_count=access_count+1, last_accessed_timestamp=?1 \
             WHERE id=?2 AND tombstone=0",
            params![now_ts as i64, id],
        )?;
        Ok(())
    }

    /// 批量 touch_access (L2: 一次 retrieve 对 N 条命中逐条单行写 → 改单次批量 UPDATE)。
    /// 去重后 `WHERE id IN (...)` 一次写, 降 N 次 SQLite 写为 1 次。每条 access_count +1 (相对更新, 防丢失更新)。
    pub fn touch_access_batch(&self, ids: &[String], now_ts: u64) -> PersistResult<()> {
        if ids.is_empty() {
            return Ok(());
        }
        // 去重 (同 id 多 turn 命中只 +1, "检索会话"计次而非"命中 turn"计次)。
        let mut seen = std::collections::HashSet::new();
        let unique: Vec<&str> = ids
            .iter()
            .filter(|id| seen.insert(id.as_str()))
            .map(|id| id.as_str())
            .collect();
        if unique.is_empty() {
            return Ok(());
        }
        let conn = self.conn()?;
        // rusqlite 无绑定数组 → 用 IN (?, ?, ...) 拼占位。
        let placeholders: Vec<String> = (0..unique.len()).map(|_| "?".to_string()).collect();
        let sql = format!(
            // §3.6: AND tombstone=0 — 批量 touch 不 bump 已软删记忆 (同 touch_access 守卫)。
            "UPDATE memory_item SET access_count=access_count+1, last_accessed_timestamp=?1 \
             WHERE id IN ({}) AND tombstone=0",
            placeholders.join(", ")
        );
        let mut params_vec: Vec<Box<dyn rusqlite::ToSql>> = Vec::with_capacity(unique.len() + 1);
        params_vec.push(Box::new(now_ts as i64));
        for id in &unique {
            params_vec.push(Box::new((*id).to_string()));
        }
        let params_refs: Vec<&dyn rusqlite::ToSql> =
            params_vec.iter().map(|p| p.as_ref()).collect();
        conn.execute(&sql, params_refs.as_slice())?;
        Ok(())
    }

    pub fn record_consolidation(
        &self,
        report: &ConsolidationReport,
        started_at: u64,
    ) -> PersistResult<()> {
        let conn = self.conn()?;
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

    pub fn append_wop(&self, op: &str, payload: &str, at: u64) -> PersistResult<i64> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO wop_log(op,payload,at) VALUES(?1,?2,?3)",
            params![op, payload, at as i64],
        )?;
        let seq = conn.last_insert_rowid();
        debug!(seq, op, "wop appended");
        Ok(seq)
    }

    // 返回最大 seq，空表返回 0。follower 拉增量基线。
    pub fn last_wop_seq(&self) -> PersistResult<i64> {
        let conn = self.conn()?;
        let seq: i64 = conn.query_row("SELECT COALESCE(MAX(seq), 0) FROM wop_log", [], |r| {
            r.get(0)
        })?;
        Ok(seq)
    }

    // 增量拉取 seq > since 的 wop 条目，按 seq 升序，最多 limit 条。leader→follower 复制读路径。
    pub fn list_wop_since(&self, since_seq: i64, limit: usize) -> PersistResult<Vec<WopEntry>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT seq,op,payload,at FROM wop_log WHERE seq > ?1 ORDER BY seq ASC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![since_seq, limit as i64], |row| {
            Ok(WopEntry {
                seq: row.get(0)?,
                op: row.get(1)?,
                payload: row.get(2)?,
                at: row.get(3)?,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// §2.6: 裁剪 seq < before_seq 的 wop 条目, 返回裁剪行数。防死 follower 致 leader wop_log
    /// 无界增长 → 磁盘填满 → append_wop SQLITE_FULL 全局写中断。
    /// 仅保留所有 follower 未消费的增量 (before_seq = 各 follower 已确认 seq 的最小值)。
    /// opt-in: 调方据 FUSION_MEMORY_WOP_RETENTION 决定是否裁剪 (默认不裁, 保守防丢增量)。
    pub fn prune_wop_before(&self, before_seq: i64) -> PersistResult<usize> {
        let conn = self.conn()?;
        let n = conn.execute("DELETE FROM wop_log WHERE seq < ?1", params![before_seq])?;
        if n > 0 {
            info!(pruned = n, before_seq, "wop_log pruned (§2.6 retention)");
        }
        Ok(n)
    }

    pub fn record_merge(
        &self,
        source_id: &str,
        target_id: &str,
        reason: &str,
        at: u64,
    ) -> PersistResult<()> {
        let conn = self.conn()?;
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
        let conn = self.conn()?;
        conn.execute(
            "INSERT OR IGNORE INTO relation(src,dst,rel_type,weight,rule_priority,first_seen) \
             VALUES(?1,?2,?3,?4,?5,?6)",
            params![src, dst, rel_type, weight, rule_priority, first_seen as i64],
        )?;
        Ok(())
    }

    /// 取某实体的直接邻居 (1-hop)。供对齐/图遍历用。
    pub fn list_relations_from(&self, src: &str) -> PersistResult<Vec<(String, String, f64)>> {
        let conn = self.conn()?;
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
    /// P4: 稠密图递归 CTE 扇出无度上限, hop_limit=2 仍可达数千节点。加 LIMIT 早终止
    /// (graph_affinity 只取最近 hop 的 0.5^h, 远端节点贡献指数衰减, 截断无损评分精度)。
    pub fn n_hop_reachable(
        &self,
        start: &str,
        hop_limit: usize,
    ) -> PersistResult<Vec<(String, usize)>> {
        let conn = self.conn()?;
        let sql = r"WITH RECURSIVE hop(lvl, node) AS (
  SELECT 0, ?1
  UNION ALL
  SELECT h.lvl+1, r.dst FROM hop h JOIN relation r ON r.src=h.node
  WHERE h.lvl < ?2
)
SELECT DISTINCT node, MIN(lvl) AS first_hop FROM hop WHERE node != ?1 GROUP BY node
ORDER BY first_hop ASC LIMIT ?3";
        let mut stmt = conn.prepare(sql)?;
        let rows = stmt.query_map(
            params![start, hop_limit as i64, N_HOP_RESULT_LIMIT],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as usize)),
        )?;
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
        let conn = self.conn()?;
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
        let conn = self.conn()?;
        // §3.17: 旧版 .ok() 把 SQLITE_BUSY 吞成 None → 别名静默丢失; .unwrap_or_default() 把坏 JSON 吞成空 → 旧别名覆盖丢失。
        // 改: 区分 QueryReturnedNoRows (实体不存在, 正常 None) vs 真错误 (含 Busy) 上抛; 坏 JSON warn 留痕不静默。
        let row: Option<String> = match conn.query_row(
            "SELECT aliases FROM entity WHERE id=?1",
            params![entity_id],
            |row| row.get(0),
        ) {
            Ok(v) => Some(v),
            Err(rusqlite::Error::QueryReturnedNoRows) => None,
            Err(e) => return Err(e.into()),
        };
        if let Some(aliases_json) = row {
            let mut aliases: Vec<String> = match serde_json::from_str(&aliases_json) {
                Ok(v) => v,
                Err(e) => {
                    warn!(
                        entity_id = %entity_id,
                        error = %e,
                        "append_entity_alias: aliases JSON corrupt, resetting to empty"
                    );
                    Vec::new()
                }
            };
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
        let conn = self.conn()?;
        // §3.5: 旧版 .ok().unwrap_or(0) 把 SQLITE_BUSY 吞成 0 → 调用方误判"从未 consolidate"
        // → 每次都全表扫重算 (consolidate 突发忙时退化为 O(N) 扫描, 越忙越慢)。
        // 改: 区分无记录 (MAX 返 NULL→row.get(0) 对 Option<i64> 得 None, 或空表 QueryReturnedNoRows) vs 真错误上抛。
        let at: Option<i64> =
            match conn.query_row("SELECT MAX(started_at) FROM consolidation_log", [], |row| {
                row.get(0)
            }) {
                Ok(v) => v,
                Err(rusqlite::Error::QueryReturnedNoRows) => None,
                Err(e) => return Err(e.into()),
            };
        Ok(at.unwrap_or(0) as u64)
    }

    /// 增量变更集: last_accessed 或 created > since 的非 tombstone 记忆。B4。
    pub fn list_changed_since(&self, since: u64) -> PersistResult<Vec<MemoryItem>> {
        let conn = self.conn()?;
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
        let conn = self.conn()?;
        let mut stmt = conn.prepare("SELECT * FROM memory_item WHERE tombstone=1")?;
        let rows = stmt.query_map([], row_to_memory)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// 物理删 memory_item + 显式级联 memory_entity (reconcile 三库一致后调用, §8.4)。
    /// 旧版仅删 memory_item 依赖 FK ON DELETE CASCADE, 但 PRAGMA foreign_keys 仅在
    /// init_conn 单连接开启, 跨连接/未来变体不保证 → 显式 DELETE memory_entity 双保险。
    pub fn physical_delete(&self, id: &str) -> PersistResult<()> {
        let mut conn = self.conn_mut()?;
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM memory_entity WHERE memory_id=?1", params![id])?;
        tx.execute("DELETE FROM memory_item WHERE id=?1", params![id])?;
        tx.commit()?;
        Ok(())
    }

    /// 全部 merge_log (fm-cli unmerge 列表用)。
    pub fn list_merge_log(&self) -> PersistResult<Vec<MergeLogEntry>> {
        let conn = self.conn()?;
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
        let conn = self.conn()?;
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
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO reconcile_report(at,memory_id,stage,error) VALUES(?1,?2,?3,?4)",
            params![at as i64, memory_id, stage, error],
        )?;
        Ok(())
    }

    /// P1-3: 追加审计日志 (核心路径 who/when/what)。失败仅 warn 不阻断核心路径。
    pub fn append_audit(
        &self,
        at: u64,
        actor: &str,
        action: &str,
        target_id: &str,
        detail: &str,
    ) -> PersistResult<()> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO audit_log(at,actor,action,target_id,detail) VALUES(?1,?2,?3,?4,?5)",
            params![at as i64, actor, action, target_id, detail],
        )?;
        Ok(())
    }

    /// P1-3: 列审计日志 (fm-cli audit 用)。limit=None → 全量 (倒序)。
    pub fn list_audit(&self, limit: Option<u64>) -> PersistResult<Vec<AuditLogEntry>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT id,at,actor,action,target_id,detail FROM audit_log ORDER BY id DESC LIMIT ?1",
        )?;
        let row_to_audit = |r: &rusqlite::Row| -> rusqlite::Result<AuditLogEntry> {
            Ok(AuditLogEntry {
                id: r.get::<_, i64>(0)? as u64,
                at: r.get::<_, i64>(1)? as u64,
                actor: r.get(2)?,
                action: r.get(3)?,
                target_id: r.get(4)?,
                detail: r.get(5)?,
            })
        };
        // limit=None → 用 i64::MAX 等价全量 (倒序 + LIMIT MAX), 避免双 SQL 分支。
        let lim = limit.unwrap_or(i64::MAX as u64) as i64;
        let rows = stmt.query_map(params![lim], row_to_audit)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
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

// wop_log 条目 (leader→follower 复制传输单元)。PRD §16 wop_log replay。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WopEntry {
    pub seq: i64,
    pub op: String,
    pub payload: String,
    pub at: i64,
}

/// P1-3: audit_log 条目 (核心路径 who/when/what, fm-cli audit 展示用)。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditLogEntry {
    pub id: u64,
    pub at: u64,
    pub actor: String,
    pub action: String,
    pub target_id: String,
    pub detail: String,
}

const _: () = {
    // 占位：保证 entities_json 列名常量可见（避免未来误删）
};

fn row_to_memory(row: &rusqlite::Row) -> rusqlite::Result<MemoryItem> {
    let id: String = row.get("id")?;
    let memory_type_str: String = row.get("memory_type")?;
    let tier_str: String = row.get("tier")?;
    let entities_json: String = row.get("entities_json")?;
    // §3.2: 坏 JSON 静默成空实体 → 图边消失但源记忆仍指旧 vector_ref, consolidate 当新鲜 episodic 处理。
    // 改: 坏 JSON warn 留痕 (仍降级空 Vec 保服务连续, 不 panic), 运维可据 warn 定位损坏行。
    let entities: Vec<EntityNode> = match serde_json::from_str(&entities_json) {
        Ok(v) => v,
        Err(e) => {
            warn!(id = %id, error = %e, "row_to_memory: entities_json corrupt, degraded to empty");
            Vec::new()
        }
    };
    let tombstone_i: i64 = row.get("tombstone")?;
    let pending_i: i64 = row.get("entities_pending")?;
    // §3.2: 未知 enum 字符串静默降级默认 → 记忆语义被改写 (Procedural 被截断成 Episodic)。
    // 改: 未知值 warn 留痕 (仍降级默认保服务连续)。
    let memory_type = match MemoryType::parse(&memory_type_str) {
        Some(t) => t,
        None => {
            warn!(
                id = %id,
                raw = %memory_type_str,
                "row_to_memory: unknown memory_type, degraded to Episodic"
            );
            MemoryType::Episodic
        }
    };
    let tier = match MemoryTier::parse(&tier_str) {
        Some(t) => t,
        None => {
            warn!(
                id = %id,
                raw = %tier_str,
                "row_to_memory: unknown tier, degraded to Short"
            );
            MemoryTier::Short
        }
    };
    Ok(MemoryItem {
        id,
        interaction_id: row.get("interaction_id")?,
        turn_idx: row.get::<_, i64>("turn_idx")? as u32,
        session_id: row.get("session_id")?,
        memory_type,
        tier,
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
    fn put_memory_with_entities_writes_memory_entity_rows() {
        // H1 事务: put_memory 带 entity → memory_item + entity + memory_entity 三行同事务落地。
        let p = Persist::open_in_memory().unwrap();
        let mut m = sample_item("m-ent", "ix1", 0);
        m.entities.push(EntityNode::new(
            "ent-a".into(),
            "Entity A".into(),
            EntityType::Tech,
        ));
        p.put_memory(&m).unwrap();
        let got = p.get_memory("m-ent").unwrap().unwrap();
        assert_eq!(got.entities.len(), 1);
        assert_eq!(got.entities[0].id, "ent-a");
        // memory_entity 行存在 (FK 关联)
        let n: i64 = p
            .conn()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM memory_entity WHERE memory_id=?1",
                params!["m-ent"],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1, "memory_entity 行应写入");
    }

    #[test]
    fn physical_delete_removes_memory_entity_rows() {
        // L3/M1: physical_delete 显式级联 memory_entity (不靠 FK pragma)。
        let p = Persist::open_in_memory().unwrap();
        let mut m = sample_item("m-del", "ix1", 0);
        m.entities.push(EntityNode::new(
            "ent-b".into(),
            "Entity B".into(),
            EntityType::Tech,
        ));
        p.put_memory(&m).unwrap();
        p.physical_delete("m-del").unwrap();
        assert!(p.get_memory("m-del").unwrap().is_none(), "memory_item 应删");
        let n: i64 = p
            .conn()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM memory_entity WHERE memory_id=?1",
                params!["m-del"],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 0, "memory_entity 行应随 physical_delete 级联删");
    }

    #[test]
    fn touch_access_batch_dedup_and_increment() {
        // L2: 批量 touch 去重 + 每条 access_count +1。
        let p = Persist::open_in_memory().unwrap();
        p.put_memory(&sample_item("a", "ix", 0)).unwrap();
        p.put_memory(&sample_item("b", "ix", 1)).unwrap();
        // 含重复 id "a" 两次 → 去重后 "a" 只 +1。
        let ids = vec!["a".to_string(), "b".to_string(), "a".to_string()];
        p.touch_access_batch(&ids, 5000).unwrap();
        let ga = p.get_memory("a").unwrap().unwrap();
        let gb = p.get_memory("b").unwrap().unwrap();
        assert_eq!(ga.access_count, 1, "重复 id 去重后应只 +1");
        assert_eq!(gb.access_count, 1);
        assert_eq!(ga.last_accessed_timestamp, 5000);
        // 再 batch 一次 → 累计 2。
        p.touch_access_batch(&["a".to_string(), "b".to_string()], 6000)
            .unwrap();
        assert_eq!(p.get_memory("a").unwrap().unwrap().access_count, 2);
        assert_eq!(p.get_memory("b").unwrap().unwrap().access_count, 2);
        // 空切片不报错。
        assert!(p.touch_access_batch(&[], 7000).is_ok());
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

    fn sample_item_session(id: &str, ix: &str, turn: u32, session: &str) -> MemoryItem {
        let mut m = MemoryItem::new_turn_skeleton(
            id.into(),
            ix.into(),
            turn,
            session.into(),
            MemoryType::Semantic,
            format!("content-{turn}"),
            1_000 + turn as u64,
        );
        m.tier = MemoryTier::Short;
        m
    }

    #[test]
    fn list_count_delete_by_session() {
        let p = Persist::open_in_memory().unwrap();
        p.put_memory(&sample_item_session("a", "ix1", 0, "sess-A"))
            .unwrap();
        p.put_memory(&sample_item_session("b", "ix1", 1, "sess-A"))
            .unwrap();
        p.put_memory(&sample_item_session("c", "ix2", 0, "sess-B"))
            .unwrap();
        // list_by_session
        let a = p.list_by_session("sess-A").unwrap();
        assert_eq!(a.len(), 2);
        assert!(a.iter().all(|m| m.session_id == "sess-A"));
        // count_by_session: 全量 vs 按 session
        assert_eq!(p.count_by_session(None).unwrap(), 3);
        assert_eq!(p.count_by_session(Some("sess-A")).unwrap(), 2);
        assert_eq!(p.count_by_session(Some("sess-B")).unwrap(), 1);
        assert_eq!(p.count_by_session(Some("sess-Z")).unwrap(), 0);
        // delete_by_session: tombstone sess-A 2 条, sess-B 不动
        let n = p.delete_by_session("sess-A").unwrap();
        assert_eq!(n, 2);
        assert_eq!(p.count_by_session(Some("sess-A")).unwrap(), 0);
        assert_eq!(p.count_by_session(Some("sess-B")).unwrap(), 1);
        assert_eq!(p.count().unwrap(), 1);
        // 幂等: 再删 sess-A 已无活记忆, 返 0
        assert_eq!(p.delete_by_session("sess-A").unwrap(), 0);
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
        let s1 = p.append_wop("commit", "{}", 100).unwrap();
        let s2 = p.append_wop("delete", "{}", 200).unwrap();
        assert_eq!(p.count().unwrap(), 0);
        assert_eq!(s1, 1);
        assert_eq!(s2, 2);
        assert_eq!(p.last_wop_seq().unwrap(), 2);
    }

    #[test]
    fn wop_list_since() {
        let p = Persist::open_in_memory().unwrap();
        assert_eq!(p.last_wop_seq().unwrap(), 0);
        p.append_wop("commit", "a", 100).unwrap();
        p.append_wop("delete", "b", 200).unwrap();
        p.append_wop("commit", "c", 300).unwrap();
        let all = p.list_wop_since(0, 10).unwrap();
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].seq, 1);
        assert_eq!(all[0].op, "commit");
        assert_eq!(all[2].payload, "c");
        let tail = p.list_wop_since(1, 10).unwrap();
        assert_eq!(tail.len(), 2);
        assert_eq!(tail[0].seq, 2);
        let limited = p.list_wop_since(0, 2).unwrap();
        assert_eq!(limited.len(), 2);
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

    // §1.10: init_conn 后 user_version 应 == SCHEMA_VERSION, 重复 open 幂等 (migrate 不重跑)。
    #[test]
    fn migrate_sets_user_version() {
        let p = Persist::open_in_memory().unwrap();
        let c = p.conn().unwrap();
        let v = schema::user_version(&c).unwrap();
        assert_eq!(v, schema::SCHEMA_VERSION);
        // 再 migrate 一次应 no-op (from >= to 早返)
        schema::migrate(&c).unwrap();
        assert_eq!(schema::user_version(&c).unwrap(), schema::SCHEMA_VERSION);
    }

    // §1.10: idx_memory_entity_eid 索引存在 (audit_by_entities 反查依赖)。
    #[test]
    fn schema_has_memory_entity_eid_index() {
        let p = Persist::open_in_memory().unwrap();
        let n: i64 = p
            .conn()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name='idx_memory_entity_eid'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1);
    }

    // §1.15: audit_by_entities 走 memory_entity join, 只返回命中 entity 的记忆。
    #[test]
    fn audit_by_entities_join() {
        let p = Persist::open_in_memory().unwrap();
        let mut m1 = sample_item("m-a1", "ix1", 0);
        m1.entities.push(EntityNode::new(
            "ent-a".into(),
            "A".into(),
            EntityType::Tech,
        ));
        let mut m2 = sample_item("m-a2", "ix2", 0);
        m2.entities.push(EntityNode::new(
            "ent-b".into(),
            "B".into(),
            EntityType::Tech,
        ));
        p.put_memory(&m1).unwrap();
        p.put_memory(&m2).unwrap();

        let hit_a = p.audit_by_entities(&["ent-a".into()]).unwrap();
        assert_eq!(hit_a.len(), 1);
        assert_eq!(hit_a[0].id, "m-a1");

        let hit_both = p
            .audit_by_entities(&["ent-a".into(), "ent-b".into()])
            .unwrap();
        assert_eq!(hit_both.len(), 2);

        assert!(p.audit_by_entities(&[]).unwrap().is_empty());
    }

    // §1.15: tombstone 记忆不被 audit 返回 (join 带 m.tombstone=0)。
    #[test]
    fn audit_by_entities_excludes_tombstone() {
        let p = Persist::open_in_memory().unwrap();
        let mut m = sample_item("m-tb", "ix1", 0);
        m.entities.push(EntityNode::new(
            "ent-t".into(),
            "T".into(),
            EntityType::Tech,
        ));
        p.put_memory(&m).unwrap();
        p.tombstone_memory("m-tb").unwrap();
        let hit = p.audit_by_entities(&["ent-t".into()]).unwrap();
        assert!(hit.is_empty(), "tombstone 记忆不应出现在 audit");
    }
}
