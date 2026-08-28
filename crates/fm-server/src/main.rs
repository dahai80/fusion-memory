//! fm-server 二进制: UDS JSON-RPC + HTTP(Bearer) 服务。PRD §11.2。
//!
//! 用法: 由 start.sh 管理。配置见 ServerConfig::from_file_or_env (P1-8):
//!   TOML 文件 (FM_CONFIG 指定路径, 或 data_dir/fusion-memory.toml) + env 覆盖。
//!   env: FM_HOME / FUSION_MEMORY_SOCK / FUSION_MEMORY_HTTP_PORT / FUSION_MEMORY_API_KEY
//!        / FUSION_MEMORY_DIM / FUSION_MEMORY_UDS_TOKEN
//!   secret 文件 (避免密钥落 env/cmdline): FUSION_MEMORY_API_KEY_FILE / FUSION_MEMORY_UDS_TOKEN_FILE
//!   FUSION_MEMORY_STUB=1 用 StubEmbedder 离线 (默认真 bge-m3)
//! B5: FUSION_MEMORY_API_KEY 未配但 HTTP 端口开 → 拒启 HTTP, 仅 UDS。
//! P1-8: 启动走 validate() 校验, 坏配置 fail-visible exit(1) 不裸跑。

use fm_server::{serve, ServeOpts, ServerConfig};
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();
    let cfg = ServerConfig::from_file_or_env();
    // P1-8: 启动校验 — 坏配置 fail-visible 退出, 不裸跑。
    if let Err(e) = cfg.validate() {
        error!(config_error = %e, "config validation failed, refusing to start");
        std::process::exit(1);
    }
    let stub = std::env::var("FUSION_MEMORY_STUB")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    info!(?cfg, stub, "fm-server starting");
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    if let Err(e) = rt.block_on(serve(cfg, ServeOpts { stub })) {
        error!(%e, "fm-server exited with error");
        std::process::exit(1);
    }
}
