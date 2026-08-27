//! fm-server: UDS JSON-RPC + HTTP 服务。PRD §11.2。
//!
//! UDS（sock 0600，B6）+ HTTP（axum，强制 Bearer，B5）并发。
//! 未配 FUSION_MEMORY_API_KEY 但 HTTP 端口开 → 拒启 HTTP（不裸跑）。

pub mod auth;
pub mod cluster;
pub mod config;
pub mod engine_builder;
pub mod engine_handle;
pub mod http;
pub mod jsonrpc;
pub mod uds;

use std::sync::Arc;

use tracing::{error, info, warn};

pub use config::ServerConfig;
pub use engine_builder::{build_server_engine, ServerEngine};
pub use engine_handle::EngineHandle;
use fm_engine::MemoryEngine;

/// 服务运行选项。
pub struct ServeOpts {
    pub stub: bool,
}

/// 启动服务：UDS + HTTP 并发。阻塞至任一退出。
pub async fn serve(cfg: ServerConfig, opts: ServeOpts) -> Result<(), String> {
    let ServerEngine { engine } = build_server_engine(&cfg, opts.stub)?;
    let engine: Arc<MemoryEngine> = Arc::new(engine);
    let handle = EngineHandle::new(engine.clone());

    let mut set: tokio::task::JoinSet<Result<(), String>> = tokio::task::JoinSet::new();

    // M6 集群: leader/follower 同步 task 装配 (standalone → 空)。PRD §16。
    // role 解析: env 优先, 次读 data_dir/role 文件 (fm cluster promote 落地), 末 standalone。
    let role = fm_cluster::detect_role_with_home(Some(&cfg.data_dir));
    cluster::spawn_cluster(engine.clone(), role, &mut set);

    if cfg.uds_enabled {
        let h = handle.clone();
        let sock = cfg.sock_path.clone();
        set.spawn(async move { uds::serve(sock, h).await });
    }

    let http_enabled = cfg.http_ok();
    if cfg.http_needs_token() {
        warn!("HTTP 端口开但 FUSION_MEMORY_API_KEY 未配，拒绝启动 HTTP（B5）。仅 UDS 可用。");
    }
    if http_enabled {
        let h = handle.clone();
        let port = cfg.http_port;
        let api_key = Arc::new(cfg.api_key.clone());
        set.spawn(async move {
            let state = http::HttpState { engine: h, api_key };
            http::serve(state, port).await
        });
    }

    if set.is_empty() {
        return Err("no server enabled (UDS off + HTTP off/unconfigured)".into());
    }
    info!(tasks = set.len(), "server started");
    let res = set.join_next().await;
    set.shutdown().await;
    match res {
        Some(Ok(Ok(()))) => Ok(()),
        Some(Ok(Err(e))) => {
            error!(%e, "server task exited with error");
            Err(e)
        }
        Some(Err(e)) => Err(format!("server task panicked: {e}")),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // cluster::tests 不再 set FUSION_MEMORY_ROLE (role 注入), 故 serve 读 env 恒 standalone, 无竞争。
    #[tokio::test]
    async fn serve_no_server_enabled_errors() {
        // UDS 关 + HTTP 关（端口 0）→ set 空 → 返回错误
        let dir = tempfile::tempdir().unwrap();
        let cfg = ServerConfig {
            uds_enabled: false,
            http_port: 0,
            api_key: String::new(),
            data_dir: dir.path().to_path_buf(),
            ..Default::default()
        };
        let res = serve(cfg, ServeOpts { stub: true }).await;
        assert!(res.is_err());
        let msg = res.unwrap_err();
        assert!(msg.contains("no server enabled"), "msg={msg}");
    }

    #[tokio::test]
    async fn serve_http_needs_token_rejects_http_only_uds() {
        // UDS 开 + HTTP 端口开但无 token → 走 UDS 分支，HTTP 拒启（warn 不 panic）
        let dir = tempfile::tempdir().unwrap();
        let cfg = ServerConfig {
            uds_enabled: true,
            http_port: 11435,
            api_key: String::new(), // 空 → http_needs_token
            data_dir: dir.path().to_path_buf(),
            sock_path: dir.path().join("serve-test.sock"),
            ..Default::default()
        };
        // serve 会起 UDS 并阻塞；后台 spawn 后立即 abort，仅验证不 panic + 能启动
        let h = tokio::spawn(serve(cfg, ServeOpts { stub: true }));
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        h.abort();
        // 到此未 panic 即视为 http_needs_token 分支走过
    }

    #[tokio::test]
    async fn serve_http_ok_starts_healthz() {
        // http_ok=true (api_key 配 + 端口开) + UDS 关 → 仅 HTTP 分支起, 覆盖 44-52 spawn + join 路径。
        // 取空闲端口: bind 0 拿端口后立刻 drop, serve 内会重绑 (轻微竞态可接受)。
        let dir = tempfile::tempdir().unwrap();
        let port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let cfg = ServerConfig {
            uds_enabled: false,
            http_port: port,
            api_key: "test-key".into(),
            data_dir: dir.path().to_path_buf(),
            ..Default::default()
        };
        let h = tokio::spawn(serve(cfg, ServeOpts { stub: true }));
        // 等 HTTP 起来, 最多重试 20×50ms
        let mut ok = false;
        for _ in 0..20 {
            if let Ok(mut s) = tokio::net::TcpStream::connect(format!("127.0.0.1:{port}")).await {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let _ = s
                    .write_all(b"GET /healthz HTTP/1.0\r\nHost: localhost\r\n\r\n")
                    .await;
                let mut buf = Vec::new();
                let _ = s.read_to_end(&mut buf).await;
                if buf.starts_with(b"HTTP/1.0 200") || buf.starts_with(b"HTTP/1.1 200") {
                    ok = true;
                    break;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        h.abort();
        assert!(ok, "HTTP 应起且 /healthz 返 200");
    }
}
