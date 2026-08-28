//! 子命令实现。构造 MemoryEngine + dispatch。

use std::sync::Arc;

use fm_core::{FusionMemoryEngine, Interaction, RetrieveQuery};
use fm_embed::{Embedder, StubEmbedder};
use fm_engine::MemoryEngine;
use fm_persist::Persist;
use fm_store::LocalStore;
use tracing::info;

use crate::paths::resolve_home;
use crate::{Cli, ClusterCmd, Cmd};

fn build_engine(home: &Option<String>, dim: usize) -> Result<MemoryEngine, String> {
    let dir = resolve_home(home);
    std::fs::create_dir_all(&dir).map_err(|e| format!("create home dir: {e}"))?;
    let store_dir = dir.join("store");
    let db_path = dir.join("memory.db");
    let store =
        Arc::new(LocalStore::open(&store_dir, dim).map_err(|e| format!("store open: {e}"))?);
    let persist = Arc::new(Persist::open(&db_path).map_err(|e| format!("persist open: {e}"))?);
    let embedder: Arc<dyn Embedder> = Arc::new(StubEmbedder::new(dim));
    Ok(MemoryEngine::new(store, persist, embedder))
}

pub async fn run(cli: &Cli) -> Result<(), String> {
    let engine = build_engine(&cli.home, cli.dim)?;
    match &cli.cmd {
        Cmd::Commit { session, file } => commit(&engine, session, file).await,
        Cmd::Query {
            text,
            top_k,
            budget,
        } => query(&engine, text, *top_k, *budget).await,
        Cmd::Stats => stats(&engine).await,
        Cmd::Delete { id, confirm } => delete(&engine, id, *confirm).await,
        Cmd::Doctor => doctor(&engine).await,
        Cmd::Consolidate => consolidate(&engine).await,
        Cmd::Merges => merges(&engine).await,
        Cmd::Unmerge { id } => unmerge(&engine, *id).await,
        Cmd::Reconcile => reconcile(&engine).await,
        Cmd::Import { source, stub } => import(&cli.home, source, *stub).await,
        Cmd::Cluster { sub } => cluster(&cli.home, sub).await,
        Cmd::Backup { dest } => backup(&cli.home, cli.dim, dest).await,
        Cmd::Restore { source, confirm } => restore(&cli.home, source, *confirm).await,
        Cmd::Audit { limit } => audit(&engine, *limit).await,
    }
}

async fn commit(engine: &MemoryEngine, session: &str, file: &Option<String>) -> Result<(), String> {
    let raw = match file {
        Some(p) => std::fs::read_to_string(p).map_err(|e| format!("read file: {e}"))?,
        None => {
            use std::io::Read;
            let mut s = String::new();
            std::io::stdin()
                .read_to_string(&mut s)
                .map_err(|e| format!("read stdin: {e}"))?;
            s
        }
    };
    let ix: Interaction = serde_json::from_str(&raw).map_err(|e| format!("parse json: {e}"))?;
    let ids = engine
        .commit_episodic_memory(session, &ix)
        .await
        .map_err(|e| format!("commit: {e}"))?;
    println!("committed {} turns:", ids.len());
    for id in &ids {
        println!("  {}", id.as_str());
    }
    Ok(())
}

async fn query(
    engine: &MemoryEngine,
    text: &str,
    top_k: usize,
    budget: usize,
) -> Result<(), String> {
    let q = RetrieveQuery::new(text, top_k, budget);
    let ctx = engine
        .retrieve_context(&q)
        .await
        .map_err(|e| format!("query: {e}"))?;
    println!("blocks: {}  tokens: {}", ctx.blocks.len(), ctx.total_tokens);
    for (i, b) in ctx.blocks.iter().enumerate() {
        println!(
            "--- block {i} interaction={} score={:.3} type={:?} turns={}",
            b.interaction_id,
            b.score,
            b.memory_type,
            b.turns.len()
        );
        println!("{}", b.turns_text);
    }
    Ok(())
}

async fn stats(engine: &MemoryEngine) -> Result<(), String> {
    let n = engine
        .persist()
        .count()
        .map_err(|e| format!("count: {e}"))?;
    println!("active memories: {n}");
    Ok(())
}

async fn delete(engine: &MemoryEngine, id: &str, confirm: bool) -> Result<(), String> {
    if !confirm {
        return Err("delete requires --confirm".into());
    }
    engine
        .delete_memory(id)
        .await
        .map_err(|e| format!("delete: {e}"))?;
    println!("tombstoned: {id}");
    Ok(())
}

async fn doctor(engine: &MemoryEngine) -> Result<(), String> {
    let store_ok = engine.store().dimension() > 0;
    let n = engine
        .persist()
        .count()
        .map_err(|e| format!("count: {e}"))?;
    println!("doctor:");
    println!("  store:  {}", if store_ok { "ok" } else { "fail" });
    println!("  persist: ok");
    println!("  memories: {n}");
    info!("doctor done");
    Ok(())
}

async fn consolidate(engine: &MemoryEngine) -> Result<(), String> {
    let report = engine
        .consolidate_memories()
        .await
        .map_err(|e| format!("consolidate: {e}"))?;
    println!("consolidate done in {} ms:", report.elapsed_ms);
    println!(
        "  dropped: {}  promoted: {}  merged: {}  summarized: {}  reextracted: {}  reconciled: {}",
        report.dropped,
        report.promoted,
        report.merged,
        report.summarized,
        report.reextracted,
        report.reconciled
    );
    if !report.failures.is_empty() {
        println!("  failures: {}", report.failures.len());
        for f in &report.failures {
            println!("    {} @{}: {}", f.memory_id, f.stage, f.error);
        }
    }
    info!(?report, "consolidate done");
    Ok(())
}

async fn merges(engine: &MemoryEngine) -> Result<(), String> {
    let log = engine
        .list_merges()
        .map_err(|e| format!("list merges: {e}"))?;
    println!("merges: {}", log.len());
    for m in &log {
        println!(
            "  id={}  {} -> {}  reason={}",
            m.id, m.source_id, m.target_id, m.reason
        );
    }
    Ok(())
}

// P1-3: 列审计日志 (commit/retrieve/consolidate/delete who/when/what)。
async fn audit(engine: &MemoryEngine, limit: u64) -> Result<(), String> {
    let log = engine
        .persist()
        .list_audit(Some(limit))
        .map_err(|e| format!("list audit: {e}"))?;
    println!("audit (last {}): {}", limit, log.len());
    for a in &log {
        println!(
            "  id={}  at={}  actor={}  action={}  target={}  detail={}",
            a.id, a.at, a.actor, a.action, a.target_id, a.detail
        );
    }
    Ok(())
}

async fn unmerge(engine: &MemoryEngine, id: u64) -> Result<(), String> {
    let ok = engine
        .unmerge(id)
        .await
        .map_err(|e| format!("unmerge: {e}"))?;
    if ok {
        println!("unmerged: {id} (source restored)");
    } else {
        return Err(format!("unmerge id {id} not found or source missing"));
    }
    Ok(())
}

async fn reconcile(engine: &MemoryEngine) -> Result<(), String> {
    let n = engine.reconcile().map_err(|e| format!("reconcile: {e}"))?;
    println!("reconcile: {n} tombstone physically deleted");
    info!(n, "reconcile done");
    Ok(())
}

async fn cluster(home: &Option<String>, sub: &ClusterCmd) -> Result<(), String> {
    let dir = resolve_home(home);
    match sub {
        ClusterCmd::Status => {
            let role = fm_cluster::detect_role_with_home(Some(&dir));
            let leader = std::env::var("FUSION_MEMORY_LEADER").unwrap_or_default();
            let sync_port = std::env::var("FUSION_MEMORY_SYNC_PORT")
                .unwrap_or_else(|_| fm_cluster::ClusterConfig::default().sync_port.to_string());
            // 打开 persist 读 last_wop_seq (轻量, 仅 SQLite)。
            let db_path = dir.join("memory.db");
            let seq = match Persist::open(&db_path) {
                Ok(p) => p.last_wop_seq().map_err(|e| format!("last_wop_seq: {e}"))?,
                Err(e) => {
                    info!(error = %e, "persist open failed for cluster status, seq unknown");
                    -1
                }
            };
            println!("cluster status:");
            println!("  role:       {}", role);
            println!("  wop_seq:    {}", seq);
            println!(
                "  leader:     {}",
                if leader.is_empty() { "(none)" } else { &leader }
            );
            println!("  sync_port:  {}", sync_port);
            // v1.0.0 B-2: 选举状态 (自动 failover)。配 FUSION_MEMORY_CLUSTER_NODES → enabled。
            match fm_cluster::ElectionConfig::from_env() {
                Ok(Some(ec)) => {
                    println!(
                        "  election:   enabled ({} nodes, self_id={}, quorum={}, auto-failover on)",
                        ec.nodes.len(),
                        ec.self_id,
                        ec.quorum()
                    );
                    println!("    nodes:     {}", ec.nodes.join(", "));
                }
                Ok(None) => {
                    println!("  election:   disabled (no FUSION_MEMORY_CLUSTER_NODES, manual promote only)");
                }
                Err(e) => {
                    println!("  election:   config error: {e}");
                }
            }
            // §1.8: epoch (fencing)。读 home/epoch 文件。
            let epoch = fm_cluster::read_epoch_file(&dir);
            println!("  epoch:      {}", epoch);
            info!(%role, seq, "cluster status");
            Ok(())
        }
        ClusterCmd::Promote => {
            // 手动 failover: 写 home/role=leader, §1.8 递增 epoch (fencing 旧 leader 防脑裂双写)。
            std::fs::create_dir_all(&dir).map_err(|e| format!("create home dir: {e}"))?;
            let path = fm_cluster::write_role_file(&dir, fm_cluster::NodeRole::Leader)
                .map_err(|e| format!("write role file: {e}"))?;
            // §1.8: 读旧 epoch +1 落地。新 leader 重启经 ClusterConfig::with_home 读此 epoch,
            // 自报给 follower; follower (同样读此 epoch 或 env) 拒 epoch < 此值的旧 leader。
            let old_epoch = fm_cluster::read_epoch_file(&dir);
            let new_epoch = old_epoch + 1;
            let epoch_path = fm_cluster::write_epoch_file(&dir, new_epoch)
                .map_err(|e| format!("write epoch file: {e}"))?;
            println!("promoted: role=leader written to {}", path.display());
            println!(
                "  epoch: {} -> {} (fencing, written to {})",
                old_epoch,
                new_epoch,
                epoch_path.display()
            );
            println!("next steps:");
            println!("  1. stop old leader (if any): FUSION_MEMORY_ROLE unset + fm-server stop");
            println!(
                "  2. restart this node's fm-server (reads {}/role + {}/epoch)",
                dir.display(),
                dir.display()
            );
            println!(
                "  3. point followers: FUSION_MEMORY_LEADER=<this-node-addr>:{} (and set FUSION_MEMORY_CLUSTER_EPOCH={new_epoch} on followers, or share {}/epoch)",
                {
                    std::env::var("FUSION_MEMORY_SYNC_PORT").unwrap_or_else(|_| {
                        fm_cluster::ClusterConfig::default().sync_port.to_string()
                    })
                },
                dir.display()
            );
            info!(path = %path.display(), old_epoch, new_epoch, "cluster promote done");
            Ok(())
        }
    }
}

async fn import(home: &Option<String>, source: &Option<String>, stub: bool) -> Result<(), String> {
    let source_db = match source {
        Some(p) => p.clone(),
        None => {
            let h = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
            format!("{h}/.fusion-agent-studio/memory.db")
        }
    };
    if !std::path::Path::new(&source_db).exists() {
        return Err(format!("source db not found: {source_db}"));
    }
    let engine = crate::import_studio::build_import_engine(home, stub)?;
    let report = crate::import_studio::run_import(&engine, &source_db).await?;
    println!("import from {source_db}");
    println!(
        "  imported: {}  skipped_archive: {}  skipped_empty: {}  failed: {}",
        report.imported, report.skipped_archive, report.skipped_empty, report.failed
    );
    Ok(())
}

// P0-4: 备份 — SQLite VACUUM INTO 一致快照 + sled store 目录整体拷贝。
// 写入 <dest>/memory.db (单文件可移植) + <dest>/store/ (向量目录)。
async fn backup(home: &Option<String>, dim: usize, dest: &Option<String>) -> Result<(), String> {
    let dir = resolve_home(home);
    if !dir.exists() {
        return Err(format!(
            "home dir not found: {} (nothing to back up)",
            dir.display()
        ));
    }
    let db_path = dir.join("memory.db");
    if !db_path.exists() {
        return Err(format!(
            "memory.db not found in {} (run commit first)",
            dir.display()
        ));
    }
    let dest_dir = match dest {
        Some(d) => std::path::PathBuf::from(d),
        None => {
            // 默认 <home>/backups/<unix_millis>。用 SystemTime 取时间戳 (CLI 侧可用)。
            let ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            dir.join("backups").join(ts.to_string())
        }
    };
    if dest_dir.exists() {
        return Err(format!(
            "backup dest already exists: {} (refusing overwrite)",
            dest_dir.display()
        ));
    }
    std::fs::create_dir_all(&dest_dir).map_err(|e| format!("create dest dir: {e}"))?;

    // 1. SQLite 一致快照 (VACUUM INTO, 在线不阻塞写)。
    let persist = Persist::open(&db_path).map_err(|e| format!("persist open: {e}"))?;
    let dest_db = dest_dir.join("memory.db");
    let db_bytes = persist
        .backup_sqlite(&dest_db)
        .map_err(|e| format!("sqlite backup: {e}"))?;

    // 2. sled store 目录整体拷贝 (向量索引, 二进制树)。
    let src_store = dir.join("store");
    let dest_store = dest_dir.join("store");
    let mut vec_count = 0u64;
    if src_store.exists() {
        copy_dir_recursive(&src_store, &dest_store, &mut vec_count)
            .map_err(|e| format!("copy store dir: {e}"))?;
    }

    println!("backup done -> {}", dest_dir.display());
    println!("  memory.db: {} bytes", db_bytes);
    println!("  store/: {} files copied (dim hint={})", vec_count, dim);
    info!(dest = %dest_dir.display(), db_bytes, vec_count, dim, "backup complete");
    Ok(())
}

// P0-4: 恢复 — 从备份目录覆盖现数据目录。破坏性, 需 confirm=true + fm-server 未运行。
async fn restore(home: &Option<String>, source: &str, confirm: bool) -> Result<(), String> {
    if !confirm {
        return Err(
            "restore is destructive: pass --confirm to acknowledge overwrite of live data dir"
                .into(),
        );
    }
    let src_dir = std::path::PathBuf::from(source);
    if !src_dir.exists() {
        return Err(format!("backup source not found: {}", src_dir.display()));
    }
    let src_db = src_dir.join("memory.db");
    let src_store = src_dir.join("store");
    if !src_db.exists() {
        return Err(format!(
            "no memory.db in backup source {} (not a valid backup dir)",
            src_dir.display()
        ));
    }

    let dir = resolve_home(home);

    // 安全门: fm-server 在跑则拒绝 (单写者, 否则覆盖瞬间被写回)。
    // 探 sock 文件存在 + 可连即视为运行中。
    let sock = dir.join("fm.sock");
    if sock.exists() {
        // 试连 UDS; 连上说明 server 活着。
        if let Ok(s) = std::os::unix::net::UnixStream::connect(&sock) {
            drop(s);
            return Err(format!(
                "fm-server appears running (sock {} live). stop it first before restore",
                sock.display()
            ));
        }
    }

    // 清空目标 memory.db + store/, 再从备份覆盖。
    std::fs::create_dir_all(&dir).map_err(|e| format!("create home dir: {e}"))?;
    let dst_db = dir.join("memory.db");
    let dst_store = dir.join("store");
    if dst_db.exists() {
        std::fs::remove_file(&dst_db).map_err(|e| format!("remove old memory.db: {e}"))?;
    }
    if dst_store.exists() {
        std::fs::remove_dir_all(&dst_store).map_err(|e| format!("remove old store dir: {e}"))?;
    }
    std::fs::copy(&src_db, &dst_db).map_err(|e| format!("copy memory.db: {e}"))?;
    let mut vec_count = 0u64;
    if src_store.exists() {
        copy_dir_recursive(&src_store, &dst_store, &mut vec_count)
            .map_err(|e| format!("copy store dir: {e}"))?;
    }

    println!("restore done -> {}", dir.display());
    println!("  memory.db: restored from {}", src_db.display());
    println!("  store/: {} files restored", vec_count);
    println!("next: start fm-server on this home to serve restored data");
    info!(dest = %dir.display(), source = %src_dir.display(), vec_count, "restore complete");
    Ok(())
}

// 递归拷贝目录, 统计文件数。无 std 递归拷贝, 手写。
fn copy_dir_recursive(
    src: &std::path::Path,
    dst: &std::path::Path,
    count: &mut u64,
) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ft = entry.file_type()?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if ft.is_dir() {
            copy_dir_recursive(&from, &to, count)?;
        } else if ft.is_file() {
            std::fs::copy(&from, &to)?;
            *count += 1;
        }
        // 符号链接跳过 (sled 无 symlink, 安全忽略)。
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use fm_core::{Interaction, ToolCall, Turn};

    fn unique_home() -> String {
        use std::sync::atomic::{AtomicU64, Ordering};
        static C: AtomicU64 = AtomicU64::new(0);
        let n = C.fetch_add(1, Ordering::SeqCst);
        std::env::temp_dir()
            .join(format!("fm-cli-test-{}-{n}", std::process::id()))
            .to_string_lossy()
            .to_string()
    }

    fn build(dim: usize) -> MemoryEngine {
        let home = unique_home();
        build_engine(&Some(home), dim).unwrap()
    }

    fn interaction_json(ix: &str) -> String {
        let ix = Interaction {
            id: ix.into(),
            session_id: "s".into(),
            turns: vec![Turn {
                turn_idx: 0,
                user_message: "hello rust".into(),
                assistant_message: "hi".into(),
                tool_calls: vec![ToolCall {
                    name: "grep".into(),
                    args: serde_json::json!({}),
                    result_summary: "ok".into(),
                }],
            }],
            timestamp: 1,
            metadata: serde_json::json!({}),
        };
        serde_json::to_string(&ix).unwrap()
    }

    #[tokio::test]
    async fn commit_then_stats_then_query() {
        let eng = build(32);
        let raw = interaction_json("ix-cli-1");
        let ix: Interaction = serde_json::from_str(&raw).unwrap();
        let ids = eng.commit_episodic_memory("s", &ix).await.unwrap();
        assert_eq!(ids.len(), 1);
        let n = eng.persist().count().unwrap();
        assert_eq!(n, 1);
        let q = RetrieveQuery::new("hello rust", 10, 4096);
        let ctx = eng.retrieve_context(&q).await.unwrap();
        assert_eq!(ctx.blocks.len(), 1);
    }

    #[tokio::test]
    async fn delete_requires_confirm() {
        let eng = build(32);
        let ix: Interaction = serde_json::from_str(&interaction_json("ix-del")).unwrap();
        let ids = eng.commit_episodic_memory("s", &ix).await.unwrap();
        let err = delete(&eng, ids[0].as_str(), false).await;
        assert!(err.is_err());
        delete(&eng, ids[0].as_str(), true).await.unwrap();
    }

    #[tokio::test]
    async fn doctor_runs() {
        let eng = build(32);
        doctor(&eng).await.unwrap();
    }

    #[tokio::test]
    async fn commit_hundred_records() {
        let eng = build(32);
        let mut total = 0usize;
        for i in 0..50 {
            let ix = Interaction {
                id: format!("ix-bulk-{i}"),
                session_id: "bulk".into(),
                turns: vec![
                    Turn {
                        turn_idx: 0,
                        user_message: format!("turn0 msg {i}"),
                        assistant_message: "a0".into(),
                        tool_calls: vec![],
                    },
                    Turn {
                        turn_idx: 1,
                        user_message: format!("turn1 msg {i}"),
                        assistant_message: "a1".into(),
                        tool_calls: vec![],
                    },
                ],
                timestamp: i as u64,
                metadata: serde_json::json!({}),
            };
            let ids = eng.commit_episodic_memory("bulk", &ix).await.unwrap();
            total += ids.len();
        }
        assert_eq!(total, 100);
        assert_eq!(eng.persist().count().unwrap(), 100);
        // 100 条规模下检索聚合: 每个 block 应聚合该 interaction 的 2 turns。
        let q = RetrieveQuery::new("turn0 msg 0", 10, 8192);
        let ctx = eng.retrieve_context(&q).await.unwrap();
        assert!(!ctx.blocks.is_empty());
        for b in &ctx.blocks {
            assert_eq!(
                b.turns.len(),
                2,
                "block {} not aggregated to 2 turns",
                b.interaction_id
            );
        }
    }

    // 确认 clap Cli 可解析（防止子命令签名回退）
    #[test]
    fn cli_parses_query() {
        let args = Cli::try_parse_from(["fm", "query", "--text", "hi", "--top-k", "5"]);
        assert!(args.is_ok());
        let cli = args.unwrap();
        match cli.cmd {
            Cmd::Query { text, top_k, .. } => {
                assert_eq!(text, "hi");
                assert_eq!(top_k, 5);
            }
            _ => panic!("wrong cmd"),
        }
    }

    #[tokio::test]
    async fn commit_via_file_then_stats_and_query() {
        let eng = build(32);
        let dir = std::env::temp_dir().join(format!("fm-cli-file-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("ix.json");
        std::fs::write(&path, interaction_json("ix-file")).unwrap();
        commit(&eng, "s", &Some(path.to_string_lossy().to_string()))
            .await
            .unwrap();
        stats(&eng).await.unwrap();
        query(&eng, "hello rust", 10, 4096).await.unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn commit_bad_json_errors() {
        let eng = build(32);
        let dir = std::env::temp_dir().join(format!("fm-cli-bad-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("bad.json");
        std::fs::write(&path, "not json").unwrap();
        let err = commit(&eng, "s", &Some(path.to_string_lossy().to_string())).await;
        assert!(err.is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn commit_missing_file_errors() {
        let eng = build(32);
        let err = commit(&eng, "s", &Some("/nonexistent-fm-xyz-999".into())).await;
        assert!(err.is_err());
    }

    #[tokio::test]
    async fn build_engine_creates_dirs() {
        let home = unique_home();
        let eng = build_engine(&Some(home.clone()), 8).unwrap();
        assert!(std::path::Path::new(&home).exists());
        assert!(eng.store().dimension() == 8);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[tokio::test]
    async fn run_dispatch_stats() {
        let home = unique_home();
        let cli = Cli {
            home: Some(home.clone()),
            dim: 16,
            cmd: Cmd::Stats,
        };
        run(&cli).await.unwrap();
        let _ = std::fs::remove_dir_all(&home);
    }

    #[tokio::test]
    async fn run_dispatch_query_empty() {
        let home = unique_home();
        let cli = Cli {
            home: Some(home.clone()),
            dim: 16,
            cmd: Cmd::Query {
                text: "nothing here".into(),
                top_k: 5,
                budget: 4096,
            },
        };
        run(&cli).await.unwrap();
        let _ = std::fs::remove_dir_all(&home);
    }

    // ---- M3 命令测试 ----

    async fn semantic_item_cli(eng: &MemoryEngine, id: &str, content: &str, entity_id: &str) {
        use fm_core::{MemoryItem, MemoryTier, MemoryType};
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(1_000_000_000);
        let mut m = MemoryItem::new_turn_skeleton(
            id.into(),
            format!("ix-{id}"),
            0,
            "sess-1".into(),
            MemoryType::Semantic,
            content.into(),
            now,
        );
        m.tier = MemoryTier::Long;
        m.entities.push(fm_core::EntityNode::new(
            entity_id.into(),
            entity_id.into(),
            fm_core::EntityType::Tech,
        ));
        let vec = eng.embedder().embed(content).await.unwrap();
        let vid = fm_embed::vector_id_from_ulid(id);
        eng.store().insert_vector(vid, &vec).unwrap();
        m.vector_ref = vid.to_string();
        eng.persist().put_memory(&m).unwrap();
    }

    #[tokio::test]
    async fn consolidate_runs_on_empty() {
        let eng = build(16);
        // 空库 consolidate 不 panic, 报告全 0
        consolidate(&eng).await.unwrap();
    }

    #[tokio::test]
    async fn merges_lists_and_unmerge_restores() {
        let eng = build(16);
        // 同实体同内容 → 合并
        semantic_item_cli(&eng, "m-a", "rust cargo error", "ent-rust").await;
        semantic_item_cli(&eng, "m-b", "rust cargo error", "ent-rust").await;
        consolidate(&eng).await.unwrap();
        // merges 列出 ≥1
        let log = eng.list_merges().unwrap();
        assert!(!log.is_empty());
        merges(&eng).await.unwrap();
        // unmerge 第一条 → source 恢复
        let mid = log[0].id;
        unmerge(&eng, mid).await.unwrap();
        assert!(eng.list_merges().unwrap().is_empty());
    }

    #[tokio::test]
    async fn unmerge_unknown_id_errors() {
        let eng = build(16);
        let err = unmerge(&eng, 99999).await;
        assert!(err.is_err());
    }

    #[tokio::test]
    async fn reconcile_physical_deletes_tombstone() {
        let eng = build(16);
        semantic_item_cli(&eng, "m-rd", "unique reconcile content", "ent-x").await;
        eng.persist().tombstone_memory("m-rd").unwrap();
        let n = reconcile(&eng).await;
        // reconcile 触发物理删
        assert!(eng.persist().get_memory("m-rd").unwrap().is_none());
        let _ = n;
    }

    #[tokio::test]
    async fn run_dispatch_consolidate_and_reconcile() {
        let home = unique_home();
        let cli = Cli {
            home: Some(home.clone()),
            dim: 16,
            cmd: Cmd::Consolidate,
        };
        run(&cli).await.unwrap();
        let cli2 = Cli {
            home: Some(home.clone()),
            dim: 16,
            cmd: Cmd::Reconcile,
        };
        run(&cli2).await.unwrap();
        let cli3 = Cli {
            home: Some(home.clone()),
            dim: 16,
            cmd: Cmd::Merges,
        };
        run(&cli3).await.unwrap();
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn cli_parses_m3_commands() {
        assert!(Cli::try_parse_from(["fm", "consolidate"]).is_ok());
        assert!(Cli::try_parse_from(["fm", "merges"]).is_ok());
        assert!(Cli::try_parse_from(["fm", "reconcile"]).is_ok());
        let u = Cli::try_parse_from(["fm", "unmerge", "--id", "42"]).unwrap();
        match u.cmd {
            Cmd::Unmerge { id } => assert_eq!(id, 42),
            _ => panic!("wrong cmd"),
        }
    }

    // ---- M6 cluster 命令测试 ----

    use std::sync::{Mutex, OnceLock};
    static CLUSTER_ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    fn cluster_env_lock() -> std::sync::MutexGuard<'static, ()> {
        CLUSTER_ENV_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap()
    }

    #[tokio::test]
    async fn cluster_status_standalone_no_db() {
        // env 改写串行化 (同步块, 不跨 await); role 文件固定 standalone 免 env 竞争。
        {
            let _g = cluster_env_lock();
            std::env::remove_var("FUSION_MEMORY_ROLE");
            std::env::remove_var("FUSION_MEMORY_LEADER");
        }
        let home = unique_home();
        // 无 memory.db → seq -1, role standalone, 不 panic。
        cluster(&Some(home.clone()), &ClusterCmd::Status)
            .await
            .unwrap();
        let _ = std::fs::remove_dir_all(&home);
    }

    #[tokio::test]
    async fn cluster_status_with_db_shows_seq() {
        {
            let _g = cluster_env_lock();
            std::env::remove_var("FUSION_MEMORY_ROLE");
        }
        let home = unique_home();
        let eng = build_engine(&Some(home.clone()), 16).unwrap();
        let ix: Interaction = serde_json::from_str(&interaction_json("ix-cluster")).unwrap();
        eng.commit_episodic_memory("s", &ix).await.unwrap();
        // commit → wop_log 至少 1 行, last_wop_seq >= 1
        let seq = eng.persist().last_wop_seq().unwrap();
        assert!(seq >= 1);
        cluster(&Some(home.clone()), &ClusterCmd::Status)
            .await
            .unwrap();
        let _ = std::fs::remove_dir_all(&home);
    }

    #[tokio::test]
    async fn cluster_promote_writes_role_file() {
        {
            let _g = cluster_env_lock();
            std::env::remove_var("FUSION_MEMORY_ROLE");
        }
        let home = unique_home();
        cluster(&Some(home.clone()), &ClusterCmd::Promote)
            .await
            .unwrap();
        let role = std::fs::read_to_string(std::path::Path::new(&home).join("role")).unwrap();
        assert_eq!(role, "leader");
        // 重启后 detect_role_with_home 应解析为 leader
        assert_eq!(
            fm_cluster::detect_role_with_home(Some(std::path::Path::new(&home))),
            fm_cluster::NodeRole::Leader
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn cli_parses_cluster_commands() {
        assert!(Cli::try_parse_from(["fm", "cluster", "status"]).is_ok());
        assert!(Cli::try_parse_from(["fm", "cluster", "promote"]).is_ok());
    }

    // ---- P0-4 backup/restore 命令测试 ----

    #[tokio::test]
    async fn backup_writes_memory_db_and_store() {
        // 写一条 → backup → 目标目录应有 memory.db + store/
        let home = unique_home();
        let eng = build_engine(&Some(home.clone()), 16).unwrap();
        let ix: Interaction = serde_json::from_str(&interaction_json("ix-bk")).unwrap();
        eng.commit_episodic_memory("s", &ix).await.unwrap();

        let dest = std::env::temp_dir().join(format!("fm-cli-bk-{}-{}", std::process::id(), "bk1"));
        let _ = std::fs::remove_dir_all(&dest);
        backup(
            &Some(home.clone()),
            16,
            &Some(dest.to_string_lossy().to_string()),
        )
        .await
        .unwrap();
        assert!(dest.join("memory.db").exists(), "backup memory.db missing");
        assert!(dest.join("store").exists(), "backup store dir missing");

        // 二次 backup 同 dest → 拒绝覆盖 (已存在)。
        let err = backup(
            &Some(home.clone()),
            16,
            &Some(dest.to_string_lossy().to_string()),
        )
        .await;
        assert!(err.is_err(), "backup should refuse existing dest");

        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(&dest);
    }

    #[tokio::test]
    async fn backup_default_dest_under_home_backups() {
        let home = unique_home();
        let eng = build_engine(&Some(home.clone()), 16).unwrap();
        let ix: Interaction = serde_json::from_str(&interaction_json("ix-bk2")).unwrap();
        eng.commit_episodic_memory("s", &ix).await.unwrap();

        // dest=None → <home>/backups/<ts>
        backup(&Some(home.clone()), 16, &None).await.unwrap();
        let backups_dir = std::path::Path::new(&home).join("backups");
        assert!(backups_dir.exists(), "default backups dir missing");
        let entries: Vec<_> = std::fs::read_dir(&backups_dir).unwrap().collect();
        assert!(!entries.is_empty(), "default backup dir empty");

        let _ = std::fs::remove_dir_all(&home);
    }

    #[tokio::test]
    async fn backup_missing_home_errors() {
        let home = unique_home();
        // 不 build_engine → home 不存在
        let err = backup(&Some(home.clone()), 16, &None).await;
        assert!(err.is_err());
        let _ = std::fs::remove_dir_all(&home);
    }

    #[tokio::test]
    async fn restore_without_confirm_rejected() {
        let src = std::env::temp_dir().join(format!("fm-cli-rst-src-{}", std::process::id()));
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("memory.db"), b"x").unwrap();
        let home = unique_home();
        let err = restore(&Some(home.clone()), src.to_string_lossy().as_ref(), false).await;
        assert!(err.is_err(), "restore without --confirm must be rejected");
        let _ = std::fs::remove_dir_all(&src);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[tokio::test]
    async fn restore_missing_source_errors() {
        let home = unique_home();
        let err = restore(&Some(home.clone()), "/nonexistent-restore-src-999", true).await;
        assert!(err.is_err());
        let _ = std::fs::remove_dir_all(&home);
    }

    #[tokio::test]
    async fn restore_roundtrip_overwrites_live_data() {
        // 源 home 写一条 → backup; 目标 home 写不同条 → restore 覆盖 → 目标应只剩源数据。
        let src_home = unique_home();
        let dst_home = unique_home();
        {
            let eng = build_engine(&Some(src_home.clone()), 16).unwrap();
            let ix: Interaction = serde_json::from_str(&interaction_json("ix-src")).unwrap();
            eng.commit_episodic_memory("s", &ix).await.unwrap();
            assert_eq!(eng.persist().count().unwrap(), 1);
        }
        {
            let eng = build_engine(&Some(dst_home.clone()), 16).unwrap();
            let ix = Interaction {
                id: "ix-dst".into(),
                session_id: "s".into(),
                turns: vec![Turn {
                    turn_idx: 0,
                    user_message: "dst only content".into(),
                    assistant_message: "a".into(),
                    tool_calls: vec![],
                }],
                timestamp: 1,
                metadata: serde_json::json!({}),
            };
            eng.commit_episodic_memory("s", &ix).await.unwrap();
            assert_eq!(eng.persist().count().unwrap(), 1);
        }

        let dest = std::env::temp_dir().join(format!("fm-cli-rt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dest);
        backup(
            &Some(src_home.clone()),
            16,
            &Some(dest.to_string_lossy().to_string()),
        )
        .await
        .unwrap();

        // restore 覆盖 dst_home (无 server 运行, sock 不存在)
        restore(
            &Some(dst_home.clone()),
            dest.to_string_lossy().as_ref(),
            true,
        )
        .await
        .unwrap();

        // 重开 dst_home → count 仍 1, 但内容应是源 (ix-src)。验 memory_item id。
        let eng = build_engine(&Some(dst_home.clone()), 16).unwrap();
        assert_eq!(eng.persist().count().unwrap(), 1);
        // 源 interaction id = ix-src → list_by_interaction 应命中 (证明 dst 现持源数据)。
        let src_items = eng.persist().list_by_interaction("ix-src").unwrap();
        assert!(
            !src_items.is_empty(),
            "dst should hold src (ix-src) data after restore"
        );
        // dst 原始 ix-dst 应已不存在 (被覆盖)。
        let dst_items = eng.persist().list_by_interaction("ix-dst").unwrap();
        assert!(
            dst_items.is_empty(),
            "dst original ix-dst should be gone after restore"
        );

        let _ = std::fs::remove_dir_all(&src_home);
        let _ = std::fs::remove_dir_all(&dst_home);
        let _ = std::fs::remove_dir_all(&dest);
    }

    #[test]
    fn cli_parses_backup_restore_commands() {
        assert!(Cli::try_parse_from(["fm", "backup"]).is_ok());
        assert!(Cli::try_parse_from(["fm", "backup", "--dest", "/tmp/bk"]).is_ok());
        let r = Cli::try_parse_from(["fm", "restore", "--source", "/tmp/bk", "--confirm"]).unwrap();
        match r.cmd {
            Cmd::Restore { source, confirm } => {
                assert_eq!(source, "/tmp/bk");
                assert!(confirm);
            }
            _ => panic!("wrong cmd"),
        }
    }

    #[tokio::test]
    async fn run_audit_lists_empty_log() {
        let home = unique_home();
        let cli = Cli {
            home: Some(home.clone()),
            dim: 16,
            cmd: Cmd::Audit { limit: 10 },
        };
        run(&cli).await.unwrap();
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn cli_parses_audit_command() {
        assert!(Cli::try_parse_from(["fm", "audit"]).is_ok());
        let a = Cli::try_parse_from(["fm", "audit", "--limit", "5"]).unwrap();
        match a.cmd {
            Cmd::Audit { limit } => assert_eq!(limit, 5),
            _ => panic!("wrong cmd"),
        }
    }
}
