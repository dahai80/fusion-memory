//! UDS JSON-RPC 服务。PRD §11.2 B6 修正。
//!
//! sock 文件权限 0600（限本用户）。行协议：每行一个 JSON-RPC request → 回一行 response。
//! 启动前清残留 sock 文件（避免前次异常退出残留）。

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tracing::{info, warn};

use crate::engine_handle::EngineHandle;
use crate::jsonrpc::{dispatch, parse_line, RpcResponse};

/// 清残留 sock（避免 bind 失败）。
pub fn cleanup_sock(path: &Path) {
    if path.exists() {
        if let Err(e) = std::fs::remove_file(path) {
            warn!(%e, path = ?path, "remove stale sock");
        }
    }
}

/// 启动 UDS 监听。sock 权限 0600。
pub async fn serve(sock_path: PathBuf, engine: EngineHandle) -> Result<(), String> {
    cleanup_sock(&sock_path);
    if let Some(parent) = sock_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir sock dir: {e}"))?;
    }
    let listener = UnixListener::bind(&sock_path).map_err(|e| format!("bind sock: {e}"))?;
    // 权限 0600：限本用户
    std::fs::set_permissions(&sock_path, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| format!("chmod sock: {e}"))?;
    info!(path = ?sock_path, "uds server listening (0600)");
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                let eng = engine.clone();
                tokio::spawn(handle_conn(stream, eng));
            }
            Err(e) => {
                warn!(%e, "uds accept");
                continue;
            }
        }
    }
}

async fn handle_conn(stream: UnixStream, engine: EngineHandle) {
    let (r, mut w) = stream.into_split();
    let mut reader = BufReader::new(r);
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line).await {
            Ok(0) => break,
            Ok(_) => {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let resp = match parse_line(trimmed) {
                    Some(req) => dispatch(&req, &engine).await,
                    None => RpcResponse {
                        jsonrpc: "2.0".into(),
                        result: None,
                        error: Some(crate::jsonrpc::RpcError::parse_error()),
                        id: serde_json::Value::Null,
                    },
                };
                let mut out = serde_json::to_string(&resp).unwrap_or_else(|_| "{}".into());
                out.push('\n');
                if w.write_all(out.as_bytes()).await.is_err() {
                    break;
                }
            }
            Err(e) => {
                warn!(%e, "uds read");
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fm_core::{
        ConsolidationReport, FormattedContext, Interaction, MemoryId, MemoryItem, RetrieveQuery,
    };
    use tempfile::tempdir;

    struct EchoEngine;
    #[async_trait::async_trait]
    impl fm_core::FusionMemoryEngine for EchoEngine {
        async fn commit_episodic_memory(
            &self,
            _s: &str,
            ix: &Interaction,
        ) -> fm_core::MemoryResult<Vec<MemoryId>> {
            Ok(ix
                .turns
                .iter()
                .enumerate()
                .map(|(i, _)| MemoryId(format!("m{i}")))
                .collect())
        }
        async fn retrieve_context(
            &self,
            _q: &RetrieveQuery,
        ) -> fm_core::MemoryResult<FormattedContext> {
            Ok(FormattedContext {
                blocks: vec![],
                total_tokens: 0,
            })
        }
        async fn consolidate_memories(&self) -> fm_core::MemoryResult<ConsolidationReport> {
            Ok(ConsolidationReport::default())
        }
        async fn get_memory(&self, _id: &str) -> fm_core::MemoryResult<Option<MemoryItem>> {
            Ok(None)
        }
        async fn delete_memory(&self, _id: &str) -> fm_core::MemoryResult<()> {
            Ok(())
        }
        async fn audit_memory_access(
            &self,
            _e: &[String],
        ) -> fm_core::MemoryResult<Vec<MemoryItem>> {
            Ok(vec![])
        }
    }

    #[tokio::test]
    async fn uds_health_roundtrip() {
        let dir = tempdir().unwrap();
        let sock = dir.path().join("test.sock");
        let engine = EngineHandle::from_concrete(EchoEngine);
        let h = tokio::spawn(serve(sock.clone(), engine));
        // 等监听就绪
        for _ in 0..50 {
            if sock.exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let mut s = UnixStream::connect(&sock).await.unwrap();
        s.write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"health\",\"params\":{},\"id\":7}\n")
            .await
            .unwrap();
        s.flush().await.unwrap();
        let mut reader = BufReader::new(s);
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        assert!(line.contains("ok"), "line={line}");
        // 权限 0600
        let mode = std::fs::metadata(&sock).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "sock mode={:o}", mode);
        h.abort();
    }

    #[tokio::test]
    async fn uds_commit_roundtrip() {
        let dir = tempdir().unwrap();
        let sock = dir.path().join("test2.sock");
        let engine = EngineHandle::from_concrete(EchoEngine);
        let h = tokio::spawn(serve(sock.clone(), engine));
        for _ in 0..50 {
            if sock.exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let mut s = UnixStream::connect(&sock).await.unwrap();
        let req = r#"{"jsonrpc":"2.0","method":"commit","params":{"session_id":"s","interaction":{"id":"ix1","session_id":"s","turns":[{"turn_idx":0,"user_message":"hi","assistant_message":"yo","tool_calls":[]}],"timestamp":1,"metadata":{}}},"id":1}"#;
        s.write_all(req.as_bytes()).await.unwrap();
        s.write_all(b"\n").await.unwrap();
        s.flush().await.unwrap();
        let mut reader = BufReader::new(s);
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        assert!(line.contains("m0"), "line={line}");
        h.abort();
    }

    #[test]
    fn cleanup_removes_stale() {
        let dir = tempdir().unwrap();
        let sock = dir.path().join("stale.sock");
        std::fs::write(&sock, b"x").unwrap();
        assert!(sock.exists());
        cleanup_sock(&sock);
        assert!(!sock.exists());
    }

    async fn uds_ready(sock: &Path) {
        for _ in 0..50 {
            if sock.exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }

    #[tokio::test]
    async fn uds_garbage_line_returns_parse_error() {
        let dir = tempdir().unwrap();
        let sock = dir.path().join("garbage.sock");
        let engine = EngineHandle::from_concrete(EchoEngine);
        let h = tokio::spawn(serve(sock.clone(), engine));
        uds_ready(&sock).await;
        let mut s = UnixStream::connect(&sock).await.unwrap();
        // 非法行 → parse_error(-32700)；空行被跳过不响应
        s.write_all(b"\nthis-is-not-json\n").await.unwrap();
        s.flush().await.unwrap();
        let mut reader = BufReader::new(s);
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        // 空行不回包，第一行响应应是 parse_error
        assert!(line.contains("-32700"), "line={line}");
        h.abort();
    }

    #[tokio::test]
    async fn uds_multi_line_mixed_validity() {
        let dir = tempdir().unwrap();
        let sock = dir.path().join("multi.sock");
        let engine = EngineHandle::from_concrete(EchoEngine);
        let h = tokio::spawn(serve(sock.clone(), engine));
        uds_ready(&sock).await;
        let mut s = UnixStream::connect(&sock).await.unwrap();
        // 一次写两行：garbage + valid health → 两行响应
        s.write_all(
            b"garbage1\n{\"jsonrpc\":\"2.0\",\"method\":\"health\",\"params\":{},\"id\":3}\n",
        )
        .await
        .unwrap();
        s.flush().await.unwrap();
        let mut reader = BufReader::new(s);
        let mut l1 = String::new();
        reader.read_line(&mut l1).await.unwrap();
        let mut l2 = String::new();
        reader.read_line(&mut l2).await.unwrap();
        assert!(l1.contains("-32700"), "l1={l1}");
        assert!(l2.contains("ok"), "l2={l2}");
        h.abort();
    }
}
