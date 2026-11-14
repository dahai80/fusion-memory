//! 构造服务用 MemoryEngine。PRD §6.3/§11.2。
//!
//! 默认真 MlxEmbedder(bge-m3 dim=1024) + MlxEntityExtractor(Qwen3.5)。
//! stub=true 用 StubEmbedder(离线测试)，跳过 extractor。

use std::sync::Arc;

use fm_embed::{EmbedConfig, Embedder, MlxEmbedder, StubEmbedder};
use fm_engine::entity_extract::{EntityExtractor, ExtractConfig, MlxEntityExtractor};
use fm_engine::MemoryEngine;
use fm_persist::Persist;
use fm_store::LocalStore;
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
    let store =
        Arc::new(LocalStore::open(&store_dir, dim).map_err(|e| format!("store open: {e}"))?);
    let persist = Arc::new(Persist::open(&db_path).map_err(|e| format!("persist open: {e}"))?);

    // #23: TOML [embed] 作底 (cfg 解析), env 仍最高优先 (overlay_env)。
    let ecfg = if stub {
        None
    } else {
        let base = EmbedConfig {
            mlx_url: cfg.embed_mlx_url.clone(),
            embed_model: cfg.embed_model.clone(),
            api_key: cfg.mlx_api_key.clone(),
            ..EmbedConfig::default()
        };
        // #59: 全链为空时回落 fusion-mlx settings.json auth.api_key (单一事实来源)。
        let merged = base.overlay_env().with_settings_fallback();
        info!(
            mlx_url = %merged.mlx_url,
            embed_model = %merged.embed_model,
            has_key = !merged.api_key.is_empty(),
            "embedding config resolved (#23, #59 settings fallback)"
        );
        Some(merged)
    };
    let embedder: Arc<dyn Embedder> = match &ecfg {
        None => Arc::new(StubEmbedder::new(dim)),
        Some(e) => {
            let mlx = MlxEmbedder::new(e.clone()).map_err(|e| format!("mlx embedder: {e}"))?;
            Arc::new(mlx)
        }
    };

    let mut engine = MemoryEngine::new(store, persist, embedder);

    // R8/§10.4 PII 脱敏: env FUSION_MEMORY_REDACT_PII=1 开启 (guard 未落地前自带兜底)。
    if fm_engine::redact_enabled_env() {
        engine = engine.with_redact();
        info!("PII redaction enabled (R8)");
    }

    // M6 #45: 注入检测 — 本地规则层恒开 (离线 fail-safe); FUSION_MEMORY_INJECTION_URL
    // 配置时叠加 fusion-security 委托 (仅 loopback, 防线 1)。未配置 = 零外联。
    if let Ok(url) = std::env::var("FUSION_MEMORY_INJECTION_URL") {
        if let Some(scanner) = resolve_injection_scanner(Some(&url))? {
            engine = engine.with_scanner(Some(scanner));
        }
    }

    if !stub {
        // #23: extractor 与 embedder 共用同一份解析后配置 (TOML [embed] + env 覆盖)。
        let e = ecfg.as_ref().expect("non-stub 必有 embed cfg");
        let xcfg = ExtractConfig {
            mlx_url: e.mlx_url.clone(),
            api_key: e.api_key.clone(),
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

/// M6 #45: 解析注入扫描器配置 — 非空 URL 必须为 loopback (防线 1), 否则拒启。
/// 返回 None = 未配置/空串 (零外联, 仅本地规则层)。纯函数便于测试 (env 进程全局, 测试不碰)。
fn resolve_injection_scanner(
    url: Option<&str>,
) -> Result<Option<std::sync::Arc<fm_engine::trust::HttpScanner>>, String> {
    let Some(url) = url.map(str::trim).filter(|u| !u.is_empty()) else {
        return Ok(None);
    };
    if !url.starts_with("http://127.0.0.1") && !url.starts_with("http://localhost") {
        return Err(format!(
            "FUSION_MEMORY_INJECTION_URL must be loopback (got {url}); ecosystem constraint: HTTP only to 127.0.0.1"
        ));
    }
    let token = std::env::var("FUSION_MEMORY_INJECTION_TOKEN").ok();
    Ok(Some(std::sync::Arc::new(
        fm_engine::trust::HttpScanner::new(url.to_string(), token),
    )))
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

    // M6 #45 防线 1: 非回环拒启, 回环通过, 未配置零外联 (纯函数, 不碰进程 env)。
    #[test]
    fn injection_url_loopback_guard() {
        assert!(resolve_injection_scanner(None).unwrap().is_none());
        assert!(resolve_injection_scanner(Some("")).unwrap().is_none());
        assert!(resolve_injection_scanner(Some("  ")).unwrap().is_none());
        let err = match resolve_injection_scanner(Some(
            "http://10.0.0.5:8800/api/v1/agent/injection-scan",
        )) {
            Err(e) => e,
            Ok(_) => panic!("non-loopback injection url must be rejected"),
        };
        assert!(err.contains("loopback"), "err={err}");
        let ok =
            resolve_injection_scanner(Some("http://127.0.0.1:8800/api/v1/agent/injection-scan"))
                .unwrap();
        assert!(ok.is_some());
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
