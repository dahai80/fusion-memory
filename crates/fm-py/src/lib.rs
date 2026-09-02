//! fm-py: fusion-memory Python 绑定。PRD §11.3, C2 修正。
//!
//! GIL 安全：同步签名内部 `py.allow_threads` 释放 GIL，tokio runtime block_on
//! 跑 Rust future（mlx embedding/抽实体不持 GIL），Python 事件循环不冻结。
//!
//! 用法：
//! ```python
//! import fusion_memory
//! engine = fusion_memory.Engine(data_dir="~/.fusion-memory")
//! ids = engine.commit_episodic_memory(session_id, interaction_dict)
//! ctx = engine.retrieve_context(query_text, top_k=10, token_budget=2048)
//! ```

// pyo3 #[pymethods] 宏展开会在已声明 PyResult 返回类型上生成 PyErr.into() 包裹，
// 触发 clippy::useless_conversion 误报（span 落在签名返回类型）。整体豁免。
#![allow(clippy::useless_conversion)]

use std::sync::{Arc, OnceLock};

use fm_core::{FusionMemoryEngine, Interaction, RetrieveQuery};
use fm_embed::{Embedder, StubEmbedder};
use fm_engine::MemoryEngine;
use fm_persist::Persist;
use fm_store::LocalStore;
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use pyo3::types::PyString;
use tokio::runtime::Runtime;
use tracing::{error, info};

// M3: 全进程单 tokio runtime。旧版每个 Python Engine 各建 Runtime (多 worker 线程),
// N 个 Engine → N×worker 线程爆炸。改 OnceLock 共享单 runtime, 所有 PyEngine 复用。
static SHARED_RUNTIME: OnceLock<Runtime> = OnceLock::new();

fn shared_runtime() -> &'static Runtime {
    SHARED_RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("tokio runtime build (infallible config)")
    })
}

/// Python 暴露的引擎句柄。持 tokio runtime + MemoryEngine。
/// M3: runtime 改共享单例 (&'static), 不再 per-Engine 新建。
#[pyclass(name = "Engine")]
struct PyEngine {
    runtime: &'static Runtime,
    engine: Arc<MemoryEngine>,
}

fn perr(msg: impl Into<String>) -> PyErr {
    PyRuntimeError::new_err(msg.into())
}

#[pymethods]
impl PyEngine {
    /// 构造。data_dir 默认 ~/.fusion-memory。stub=True 用 StubEmbedder(离线, dim=64)。
    #[new]
    #[pyo3(signature = (data_dir=None, stub=false))]
    fn new(data_dir: Option<String>, stub: bool) -> PyResult<Self> {
        let dir = data_dir
            .map(std::path::PathBuf::from)
            .unwrap_or_else(default_dir);
        std::fs::create_dir_all(&dir).map_err(|e| perr(format!("mkdir: {e}")))?;
        let dim = if stub { 64 } else { 1024 };
        let store =
            Arc::new(LocalStore::open(dir.join("store"), dim).map_err(|e| perr(e.to_string()))?);
        let persist =
            Arc::new(Persist::open(dir.join("memory.db")).map_err(|e| perr(e.to_string()))?);
        let embedder: Arc<dyn Embedder> = if stub {
            Arc::new(StubEmbedder::new(dim))
        } else {
            let cfg = fm_embed::EmbedConfig {
                api_key: std::env::var("FUSION_MEMORY_MLX_API_KEY").unwrap_or_default(),
                ..fm_embed::EmbedConfig::from_env()
            };
            let mlx = fm_embed::MlxEmbedder::new(cfg).map_err(|e| perr(e.to_string()))?;
            Arc::new(mlx)
        };
        let mut engine = MemoryEngine::new(store, persist, embedder);
        // §1.16: PII 脱敏默认开 (redact_enabled_env 默认 true, fail-closed)。
        // 与 fm-server engine_builder 路径一致; 显式 FUSION_MEMORY_REDACT_PII=0 关闭。
        if fm_engine::redact_enabled_env() {
            engine = engine.with_redact();
            info!("PII redaction enabled (R8/§1.16, fm-py path)");
        }
        if !stub {
            let xcfg = fm_engine::entity_extract::ExtractConfig {
                mlx_url: std::env::var("FUSION_MLX_URL")
                    .unwrap_or_else(|_| "http://127.0.0.1:11434/v1".into()),
                api_key: std::env::var("FUSION_MEMORY_MLX_API_KEY").unwrap_or_default(),
                chat_model: std::env::var("FUSION_MEMORY_CHAT_MODEL")
                    .unwrap_or_else(|_| "Qwen3.5-9B-4bit".into()),
                timeout_secs: 60,
            };
            let extractor = fm_engine::entity_extract::MlxEntityExtractor::new(xcfg)
                .map_err(|e| perr(e.to_string()))?;
            engine = engine.with_extractor(Arc::new(extractor));
        }
        // M3: 复用进程级共享 runtime, 不再 per-Engine Runtime::new()。
        let runtime = shared_runtime();
        Ok(Self {
            runtime,
            engine: Arc::new(engine),
        })
    }

    /// 写入记忆，返回 turn 级 memory_id 列表。立返（同步快路径）。
    /// interaction_dict 用 dict，PyO3 转 serde。GIL 释放期间跑 Rust。
    fn commit_episodic_memory(
        &self,
        py: Python<'_>,
        session_id: &str,
        interaction_dict: Bound<'_, PyAny>,
    ) -> PyResult<Vec<String>> {
        let ix: Interaction = py_obj_to_rust(py, &interaction_dict)?;
        let engine = self.engine.clone();
        // py.allow_threads 释放 GIL，Rust future 在 tokio runtime 跑 mlx 不持 GIL。C2。
        let ids = py
            .allow_threads(move || {
                self.runtime
                    .block_on(async move { engine.commit_episodic_memory(session_id, &ix).await })
            })
            .map_err(|e| {
                error!(%e, "py commit failed");
                perr(e.to_string())
            })?;
        Ok(ids.iter().map(|i| i.0.clone()).collect())
    }

    /// 检索记忆上下文，返回 dict（FormattedContext）。
    #[pyo3(signature = (text, top_k=10, token_budget=4096, aggregate=true))]
    fn retrieve_context(
        &self,
        py: Python<'_>,
        text: &str,
        top_k: usize,
        token_budget: usize,
        aggregate: bool,
    ) -> PyResult<PyObject> {
        let q = RetrieveQuery {
            text: text.to_string(),
            top_k,
            session_id: None,
            tier_filter: None,
            token_budget,
            aggregate,
            tenant: String::new(),
        };
        let engine = self.engine.clone();
        let ctx = py
            .allow_threads(move || {
                self.runtime
                    .block_on(async move { engine.retrieve_context(&q).await })
            })
            .map_err(|e| perr(e.to_string()))?;
        rust_to_py_obj(py, &ctx)
    }

    /// 触发遗忘/合并，返回报告 dict。
    fn consolidate_memories(&self, py: Python<'_>) -> PyResult<PyObject> {
        let engine = self.engine.clone();
        let report = py
            .allow_threads(move || {
                self.runtime
                    .block_on(async move { engine.consolidate_memories().await })
            })
            .map_err(|e| perr(e.to_string()))?;
        rust_to_py_obj(py, &report)
    }
}

fn default_dir() -> std::path::PathBuf {
    if let Ok(d) = std::env::var("FM_HOME") {
        return std::path::PathBuf::from(d);
    }
    if let Some(h) = std::env::var_os("HOME") {
        return std::path::PathBuf::from(h).join(".fusion-memory");
    }
    std::path::PathBuf::from(".fusion-memory")
}

/// Python obj → T。经 Python json.dumps → str → serde 反序列化。绕过手写类型分派。
fn py_obj_to_rust<'py, T: serde::de::DeserializeOwned>(
    py: Python<'py>,
    obj: &Bound<'py, PyAny>,
) -> PyResult<T> {
    let json = py.import_bound("json")?;
    let dumped = json.getattr("dumps")?.call1((obj,))?;
    let s: String = dumped.extract()?;
    serde_json::from_str(&s).map_err(|e| perr(format!("interaction decode: {e}")))
}

/// T: Serialize → Python obj。serde 序列化 → str → Python json.loads。
fn rust_to_py_obj<T: serde::Serialize>(py: Python<'_>, val: &T) -> PyResult<PyObject> {
    let s = serde_json::to_string(val).map_err(|e| perr(e.to_string()))?;
    let json = py.import_bound("json")?;
    let py_str = PyString::new_bound(py, &s);
    Ok(json.getattr("loads")?.call1((py_str,))?.to_object(py))
}

/// Python 模块入口。
#[pymodule]
fn fusion_memory(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyEngine>()?;
    Ok(())
}

// 单元测试：dict→Interaction 往返 + value_to_py 对称（不连 mlx）。
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn value_roundtrip_dict() {
        let v = serde_json::json!({
            "id": "ix1",
            "session_id": "s",
            "turns": [{"turn_idx":0,"user_message":"hi","assistant_message":"yo","tool_calls":[]}],
            "timestamp": 1,
            "metadata": {}
        });
        let ix: Interaction = serde_json::from_value(v).unwrap();
        assert_eq!(ix.id, "ix1");
        assert_eq!(ix.turns.len(), 1);
        assert_eq!(ix.turns[0].user_message, "hi");
    }

    #[test]
    fn retrieve_query_build() {
        let q = RetrieveQuery {
            text: "x".into(),
            top_k: 5,
            session_id: None,
            tier_filter: None,
            token_budget: 100,
            aggregate: false,
            tenant: String::new(),
        };
        assert_eq!(q.top_k, 5);
        assert!(!q.aggregate);
    }

    #[test]
    fn default_dir_resolves() {
        std::env::set_var("FM_HOME", "/tmp/fm-py-test-home");
        let d = default_dir();
        assert_eq!(d, std::path::PathBuf::from("/tmp/fm-py-test-home"));
        std::env::remove_var("FM_HOME");
    }
}
