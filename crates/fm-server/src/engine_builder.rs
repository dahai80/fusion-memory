//! 构造服务用 MemoryEngine。PRD §6.3/§11.2。
//!
//! 默认真 MlxEmbedder(bge-m3 dim=1024) + MlxEntityExtractor(Qwen3.5)。
//! stub=true 用 StubEmbedder(离线测试)，跳过 extractor。

use std::sync::Arc;

use fm_embed::{EmbedConfig, Embedder, MlxEmbedder, StubEmbedder};
use fm_engine::entity_extract::{EntityExtractor, ExtractConfig, MlxEntityExtractor};
use fm_engine::MemoryEngine;
use fm_persist::Persist;
use fm_store::StoreStub;
use tracing::info;

use crate::config::ServerConfig;

/// 服务用引擎构造结果。
pub struct ServerEngine {
    pub engine: MemoryEngine,
}

/// 构造服务引擎。stub=true 离线（dim=64），否则真 bge-m3 dim=1024 + 实体抽取。
pub fn build_server_engine(cfg: &ServerConfig, stub: bool) -> Result<ServerEngine, String> {
    std::fs::create_dir_all(&cfg.data_dir).map_err(|e| format!("create data dir: {e}"))?;
    let store_dir = cfg.data_dir.join("store");
    let db_path = cfg.data_dir.join("memory.db");
    let dim = if stub { 64 } else { cfg.dim };
    let store = Arc::new(StoreStub::open(&store_dir, dim).map_err(|e| format!("store open: {e}"))?);
    let persist = Arc::new(Persist::open(&db_path).map_err(|e| format!("persist open: {e}"))?);

    let embedder: Arc<dyn Embedder> = if stub {
        Arc::new(StubEmbedder::new(dim))
    } else {
        let ecfg = EmbedConfig {
            api_key: std::env::var("FUSION_MEMORY_MLX_API_KEY").unwrap_or_default(),
            ..EmbedConfig::from_env()
        };
        let mlx = MlxEmbedder::new(ecfg).map_err(|e| format!("mlx embedder: {e}"))?;
        Arc::new(mlx)
    };

    let mut engine = MemoryEngine::new(store, persist, embedder);

    if !stub {
        let xcfg = ExtractConfig {
            mlx_url: std::env::var("FUSION_MLX_URL")
                .unwrap_or_else(|_| "http://127.0.0.1:11434/v1".into()),
            api_key: std::env::var("FUSION_MEMORY_MLX_API_KEY").unwrap_or_default(),
            chat_model: std::env::var("FUSION_MEMORY_CHAT_MODEL")
                .unwrap_or_else(|_| "Qwen3.5-9B-4bit".into()),
            timeout_secs: 60,
        };
        let extractor: Arc<dyn EntityExtractor> =
            Arc::new(MlxEntityExtractor::new(xcfg).map_err(|e| format!("extractor: {e}"))?);
        engine = engine.with_extractor(extractor);
    }
    info!(stub, dim, "server engine built");
    Ok(ServerEngine { engine })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ServerConfig;
    use tempfile::tempdir;

    #[test]
    fn build_stub_engine() {
        let dir = tempdir().unwrap();
        let cfg = ServerConfig {
            data_dir: dir.path().to_path_buf(),
            http_port: 0,
            api_key: String::new(),
            uds_enabled: false,
            ..Default::default()
        };
        let se = build_server_engine(&cfg, true).expect("stub build");
        // 引擎可调 health（不 panic 即通过，证明 store/persist/embedder 都建好）
        let _ = se.engine;
    }

    #[test]
    fn build_stub_engine_bad_dir() {
        // data_dir 指向一个已存在文件 → create_dir_all 失败
        let dir = tempdir().unwrap();
        let file = dir.path().join("afile");
        std::fs::write(&file, b"x").unwrap();
        let cfg = ServerConfig {
            data_dir: file,
            ..Default::default()
        };
        assert!(build_server_engine(&cfg, true).is_err());
    }
}
