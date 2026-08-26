//! 子命令实现。构造 MemoryEngine + dispatch。

use std::sync::Arc;

use fm_core::{FusionMemoryEngine, Interaction, RetrieveQuery};
use fm_embed::{Embedder, StubEmbedder};
use fm_engine::MemoryEngine;
use fm_persist::Persist;
use fm_store::{FusionStoreEngine, StoreStub};
use tracing::info;

use crate::paths::resolve_home;
use crate::{Cli, Cmd};

fn build_engine(home: &Option<String>, dim: usize) -> Result<MemoryEngine, String> {
    let dir = resolve_home(home);
    std::fs::create_dir_all(&dir).map_err(|e| format!("create home dir: {e}"))?;
    let store_dir = dir.join("store");
    let db_path = dir.join("memory.db");
    let store = Arc::new(StoreStub::open(&store_dir, dim).map_err(|e| format!("store open: {e}"))?);
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
        Cmd::Import { source, stub } => import(&cli.home, source, *stub).await,
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
}
