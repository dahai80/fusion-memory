//! UDS JSON-RPC 服务。PRD §11.2 B6 修正。
//!
//! sock 文件权限 0600（限本用户）。行协议：每行一个 JSON-RPC request → 回一行 response。
//! 启动前清残留 sock 文件（避免前次异常退出残留）。
//! §2.1: 行缓冲上限 + 连接数上限 (防单客户端 OOM)。

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tracing::{info, warn};

use crate::engine_handle::EngineHandle;
use crate::jsonrpc::{dispatch, parse_line, serialize_response, RpcResponse};

/// §2.1: 单行最大字节数。超过即拒绝该行 (恶意/有 bug 的 Agent 写巨大 JSON blob 不换行 → OOM)。
/// 8MB 足容 5 轮长消息 commit; 超出视为异常, 返 parse_error 不无限累积 String。
const MAX_LINE_BYTES: usize = 8 * 1024 * 1024;
/// §2.1: 最大并发 UDS 连接数。超出拒绝新连接 (防连接洪泛 OOM)。
const MAX_CONNS: usize = 256;

/// 清残留 sock（避免 bind 失败）。
pub fn cleanup_sock(path: &Path) {
    if path.exists() {
        if let Err(e) = std::fs::remove_file(path) {
            warn!(%e, path = ?path, "remove stale sock");
        }
    }
}

/// §2.1: 连接计数器, serve 持 Arc, handle_conn Drop 时递减。
struct ConnGuard {
    counter: Arc<AtomicUsize>,
}
impl Drop for ConnGuard {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::SeqCst);
    }
}

/// 启动 UDS 监听。sock 权限 0600。
/// §1.11: shutdown 信号到 → 停 accept, 在飞连接 drain (handle_conn 自然 EOF 退出)。
pub async fn serve(
    sock_path: PathBuf,
    engine: EngineHandle,
    mut shutdown: tokio::sync::oneshot::Receiver<()>,
) -> Result<(), String> {
    cleanup_sock(&sock_path);
    if let Some(parent) = sock_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir sock dir: {e}"))?;
    }
    let listener = UnixListener::bind(&sock_path).map_err(|e| format!("bind sock: {e}"))?;
    // 权限 0600：限本用户
    std::fs::set_permissions(&sock_path, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| format!("chmod sock: {e}"))?;
    info!(path = ?sock_path, "uds server listening (0600)");
    // §2.1: 全局并发连接计数, 超过 MAX_CONNS 拒新连接。
    let conn_count = Arc::new(AtomicUsize::new(0));
    // §1.11: accept 与 shutdown 竞速。用 tokio::select 让 shutdown 抢占 accept 阻塞。
    loop {
        tokio::select! {
            biased; // 优先查 shutdown
            _ = &mut shutdown => {
                info!("uds graceful shutdown triggered, stop accept");
                break;
            }
            accept = listener.accept() => {
                match accept {
                    Ok((stream, _)) => {
                        let live = conn_count.fetch_add(1, Ordering::SeqCst);
                        if live >= MAX_CONNS {
                            conn_count.fetch_sub(1, Ordering::SeqCst);
                            warn!(live, max = MAX_CONNS, "uds conn cap reached, rejecting");
                            drop(stream);
                            continue;
                        }
                        let eng = engine.clone();
                        let guard = ConnGuard {
                            counter: conn_count.clone(),
                        };
                        tokio::spawn(async move {
                            let _g = guard; // Drop 时递减计数
                            handle_conn(stream, eng).await;
                        });
                    }
                    Err(e) => {
                        warn!(%e, "uds accept");
                        continue;
                    }
                }
            }
        }
    }
    info!("uds serve exited (shutdown received)");
    Ok(())
}

async fn handle_conn(stream: UnixStream, engine: EngineHandle) {
    let (r, mut w) = stream.into_split();
    let mut reader = BufReader::new(r);
    let mut line = String::new();
    loop {
        line.clear();
        // §2.1: read_line 无界 → 限制累积字节数。超 MAX_LINE_BYTES 视为恶意/异常, 丢弃该行返 parse_error。
        match read_line_capped(&mut reader, &mut line).await {
            Ok(0) => break,
            Ok(_) => {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let resp = match parse_line(trimmed) {
                    Some(req) => dispatch(req, &engine).await,
                    None => RpcResponse {
                        jsonrpc: "2.0".into(),
                        result: None,
                        error: Some(crate::jsonrpc::RpcError::parse_error()),
                        id: serde_json::Value::Null,
                    },
                };
                // §3.18: 序列化不再 unwrap_or_else("{}")。用 serialize_response 永发合法 jsonrpc 帧。
                let mut out = serialize_response(&resp);
                out.push('\n');
                if w.write_all(out.as_bytes()).await.is_err() {
                    break;
                }
            }
            Err(LineReadError::Oversized(n)) => {
                warn!(
                    bytes = n,
                    max = MAX_LINE_BYTES,
                    "uds line over cap, rejecting"
                );
                let resp = RpcResponse {
                    jsonrpc: "2.0".into(),
                    result: None,
                    error: Some(crate::jsonrpc::RpcError::invalid_params(format!(
                        "line exceeds {} bytes",
                        MAX_LINE_BYTES
                    ))),
                    id: serde_json::Value::Null,
                };
                let mut out = serialize_response(&resp);
                out.push('\n');
                if w.write_all(out.as_bytes()).await.is_err() {
                    break;
                }
                // 继续读下一行, 不整连接断 (单行坏不应断开多路复用连接)
            }
            Err(LineReadError::Io(e)) => {
                warn!(%e, "uds read");
                break;
            }
        }
    }
}

enum LineReadError {
    Oversized(usize),
    Io(std::io::Error),
}

/// §2.1: 带字节上限的 read_line。逐块读填 line, 超 MAX_LINE_BYTES 即 Err(Oversized)。
/// 比 tokio::io::AsyncBufReadExt::read_line 多一层内存保护: 原版无界累积 String。
async fn read_line_capped(
    reader: &mut BufReader<tokio::net::unix::OwnedReadHalf>,
    line: &mut String,
) -> Result<usize, LineReadError> {
    let mut total = 0usize;
    // 用 fill_buf 逐块检查, 避免一次性 read_line 突破上限。
    loop {
        let buf = reader.fill_buf().await.map_err(LineReadError::Io)?;
        if buf.is_empty() {
            // EOF
            return Ok(total);
        }
        // 找换行
        if let Some(idx) = buf.iter().position(|&b| b == b'\n') {
            let chunk = &buf[..=idx];
            if total + chunk.len() > MAX_LINE_BYTES {
                return Err(LineReadError::Oversized(total + chunk.len()));
            }
            // 安全: chunk 含 \n, 转为 UTF-8 失败则报 io 错
            let s = std::str::from_utf8(chunk).map_err(|e| {
                LineReadError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, e))
            })?;
            line.push_str(s);
            let n = chunk.len();
            reader.consume(n);
            total += n;
            return Ok(total);
        } else {
            // 无换行, 累积整块
            if total + buf.len() > MAX_LINE_BYTES {
                return Err(LineReadError::Oversized(total + buf.len()));
            }
            let s = std::str::from_utf8(buf).map_err(|e| {
                LineReadError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, e))
            })?;
            line.push_str(s);
            let n = buf.len();
            reader.consume(n);
            total += n;
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

    /// 测试用: 返回永不触发的 shutdown RX。TX 必须存活 (drop TX 会让 oneshot RX 立即返 Err,
    /// 在 serve 的 tokio::select! 中被当完成 → serve 立即 break, 不 bind)。故用 leak 保 TX 活。
    fn never_shutdown() -> tokio::sync::oneshot::Receiver<()> {
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        std::mem::forget(tx); // 永不发, 永不 drop → RX 永远 pending
        rx
    }

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
                stale_read: false,
                last_sync_at: 0,
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
        let h = tokio::spawn(serve(sock.clone(), engine, never_shutdown()));
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
        let h = tokio::spawn(serve(sock.clone(), engine, never_shutdown()));
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
        let h = tokio::spawn(serve(sock.clone(), engine, never_shutdown()));
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
        let h = tokio::spawn(serve(sock.clone(), engine, never_shutdown()));
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

    #[tokio::test]
    async fn uds_graceful_shutdown_exits_cleanly() {
        // §1.11: shutdown 信号到 → serve 返 Ok, 不 hang。
        let dir = tempdir().unwrap();
        let sock = dir.path().join("shutdown.sock");
        let engine = EngineHandle::from_concrete(EchoEngine);
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let h = tokio::spawn(serve(sock.clone(), engine, rx));
        uds_ready(&sock).await;
        // 触发 shutdown
        tx.send(()).unwrap();
        // serve 应在 1s 内返 Ok (graceful drain, 无在飞连接即立即退)
        let res = tokio::time::timeout(std::time::Duration::from_secs(1), h).await;
        assert!(
            res.is_ok(),
            "uds serve 未在 1s 内 graceful 退出 (可能 hang)"
        );
        let outer = res.unwrap().unwrap(); // JoinHandle 结果
        assert!(outer.is_ok(), "serve 返错: {:?}", outer);
    }
}
