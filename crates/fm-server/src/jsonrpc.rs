//! JSON-RPC 2.0 共用 dispatch。UDS + HTTP 复用。PRD §11.2。
//!
//! 方法: commit/retrieve/consolidate/get/delete/audit/health。
//! delete 需 params.confirm=true（B5 二次确认）。

use fm_core::{
    ConsolidationReport, FormattedContext, Interaction, MemoryId, MemoryItem, RetrieveQuery,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::{debug, warn};

use crate::engine_handle::EngineHandle;

/// JSON-RPC 2.0 请求。
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RpcRequest {
    pub jsonrpc: String,
    pub method: String,
    #[serde(default)]
    pub params: Value,
    pub id: Value,
}

/// JSON-RPC 2.0 响应。
#[derive(Debug, Clone, Serialize)]
pub struct RpcResponse {
    pub jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
    pub id: Value,
}

/// JSON-RPC 错误。
#[derive(Debug, Clone, Serialize)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
}

impl RpcError {
    pub fn parse_error() -> Self {
        Self {
            code: -32700,
            message: "parse error".into(),
        }
    }
    pub fn method_not_found(m: &str) -> Self {
        Self {
            code: -32601,
            message: format!("method not found: {m}"),
        }
    }
    pub fn invalid_params(msg: impl Into<String>) -> Self {
        Self {
            code: -32602,
            message: msg.into(),
        }
    }
    pub fn internal(msg: impl Into<String>) -> Self {
        Self {
            code: -32603,
            message: msg.into(),
        }
    }
}

/// dispatch 单个请求。engine 经 EngineHandle 持有（Arc dyn trait）。
pub async fn dispatch(req: &RpcRequest, engine: &EngineHandle) -> RpcResponse {
    debug!(method = %req.method, "rpc dispatch");
    let res: Result<Value, RpcError> = match req.method.as_str() {
        "commit" => commit(req, engine).await,
        "retrieve" => retrieve(req, engine).await,
        "consolidate" => consolidate(engine).await,
        "get" => get(req, engine).await,
        "delete" => delete(req, engine).await,
        "audit" => audit(req, engine).await,
        "health" => Ok(Value::String("ok".into())),
        other => Err(RpcError::method_not_found(other)),
    };
    match res {
        Ok(v) => RpcResponse {
            jsonrpc: "2.0".into(),
            result: Some(v),
            error: None,
            id: req.id.clone(),
        },
        Err(e) => {
            warn!(code = e.code, msg = %e.message, "rpc error");
            RpcResponse {
                jsonrpc: "2.0".into(),
                result: None,
                error: Some(e),
                id: req.id.clone(),
            }
        }
    }
}

/// commit params。
#[derive(Debug, Deserialize)]
struct CommitParams {
    session_id: String,
    interaction: Interaction,
}

async fn commit(req: &RpcRequest, engine: &EngineHandle) -> Result<Value, RpcError> {
    let p: CommitParams = serde_json::from_value(req.params.clone())
        .map_err(|e| RpcError::invalid_params(e.to_string()))?;
    let ids: Vec<MemoryId> = engine
        .commit_episodic_memory(&p.session_id, &p.interaction)
        .await
        .map_err(|e| RpcError::internal(e.to_string()))?;
    serde_json::to_value(ids.iter().map(|i| i.0.clone()).collect::<Vec<_>>())
        .map_err(|e| RpcError::internal(e.to_string()))
}

/// retrieve params。
#[derive(Debug, Deserialize)]
struct RetrieveParams {
    text: String,
    #[serde(default = "default_top_k")]
    top_k: usize,
    #[serde(default = "default_budget")]
    token_budget: usize,
    #[serde(default = "default_true")]
    aggregate: bool,
    #[serde(default)]
    session_id: Option<String>,
}

fn default_top_k() -> usize {
    10
}
fn default_budget() -> usize {
    4096
}
fn default_true() -> bool {
    true
}

async fn retrieve(req: &RpcRequest, engine: &EngineHandle) -> Result<Value, RpcError> {
    let p: RetrieveParams = serde_json::from_value(req.params.clone())
        .map_err(|e| RpcError::invalid_params(e.to_string()))?;
    let q = RetrieveQuery {
        text: p.text,
        top_k: p.top_k,
        session_id: p.session_id,
        tier_filter: None,
        token_budget: p.token_budget,
        aggregate: p.aggregate,
    };
    let ctx: FormattedContext = engine
        .retrieve_context(&q)
        .await
        .map_err(|e| RpcError::internal(e.to_string()))?;
    serde_json::to_value(ctx).map_err(|e| RpcError::internal(e.to_string()))
}

async fn consolidate(engine: &EngineHandle) -> Result<Value, RpcError> {
    let report: ConsolidationReport = engine
        .consolidate_memories()
        .await
        .map_err(|e| RpcError::internal(e.to_string()))?;
    serde_json::to_value(report).map_err(|e| RpcError::internal(e.to_string()))
}

/// get params。
#[derive(Debug, Deserialize)]
struct GetParams {
    id: String,
}

async fn get(req: &RpcRequest, engine: &EngineHandle) -> Result<Value, RpcError> {
    let p: GetParams = serde_json::from_value(req.params.clone())
        .map_err(|e| RpcError::invalid_params(e.to_string()))?;
    let m: Option<MemoryItem> = engine
        .get_memory(&p.id)
        .await
        .map_err(|e| RpcError::internal(e.to_string()))?;
    serde_json::to_value(m).map_err(|e| RpcError::internal(e.to_string()))
}

/// delete params（confirm 必填 true，B5）。
#[derive(Debug, Deserialize)]
struct DeleteParams {
    id: String,
    #[serde(default)]
    confirm: bool,
}

async fn delete(req: &RpcRequest, engine: &EngineHandle) -> Result<Value, RpcError> {
    let p: DeleteParams = serde_json::from_value(req.params.clone())
        .map_err(|e| RpcError::invalid_params(e.to_string()))?;
    if !p.confirm {
        return Err(RpcError::invalid_params(
            "delete requires confirm=true (B5 二次确认)",
        ));
    }
    engine
        .delete_memory(&p.id)
        .await
        .map_err(|e| RpcError::internal(e.to_string()))?;
    Ok(Value::String("deleted".into()))
}

/// audit params。
#[derive(Debug, Deserialize)]
struct AuditParams {
    entity_ids: Vec<String>,
}

async fn audit(req: &RpcRequest, engine: &EngineHandle) -> Result<Value, RpcError> {
    let p: AuditParams = serde_json::from_value(req.params.clone())
        .map_err(|e| RpcError::invalid_params(e.to_string()))?;
    let ms: Vec<MemoryItem> = engine
        .audit_memory_access(&p.entity_ids)
        .await
        .map_err(|e| RpcError::internal(e.to_string()))?;
    serde_json::to_value(ms).map_err(|e| RpcError::internal(e.to_string()))
}

/// 解析单行 JSON-RPC（UDS 行协议）。
pub fn parse_line(line: &str) -> Option<RpcRequest> {
    serde_json::from_str::<RpcRequest>(line).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_request() {
        let line = r#"{"jsonrpc":"2.0","method":"health","params":{},"id":1}"#;
        let r = parse_line(line).unwrap();
        assert_eq!(r.method, "health");
        assert_eq!(r.id, Value::from(1i64));
    }

    #[test]
    fn delete_missing_confirm_rejected() {
        // 纯参数校验，不触网络。confirm=false → invalid_params。
        let req = RpcRequest {
            jsonrpc: "2.0".into(),
            method: "delete".into(),
            params: serde_json::json!({"id":"m1"}),
            id: Value::from(5i64),
        };
        let p: DeleteParams = serde_json::from_value(req.params).unwrap();
        assert!(!p.confirm);
    }

    #[test]
    fn retrieve_defaults() {
        let req = RpcRequest {
            jsonrpc: "2.0".into(),
            method: "retrieve".into(),
            params: serde_json::json!({"text":"hi"}),
            id: Value::from(1i64),
        };
        let p: RetrieveParams = serde_json::from_value(req.params).unwrap();
        assert_eq!(p.top_k, 10);
        assert_eq!(p.token_budget, 4096);
        assert!(p.aggregate);
    }

    #[test]
    fn error_codes() {
        assert_eq!(RpcError::parse_error().code, -32700);
        assert_eq!(RpcError::method_not_found("x").code, -32601);
        assert_eq!(RpcError::invalid_params("e").code, -32602);
        assert_eq!(RpcError::internal("e").code, -32603);
    }

    // ---- dispatch 全方法覆盖（StubEngine，不连 mlx）----

    use crate::engine_handle::EngineHandle;
    use fm_core::{
        ConsolidationReport, FormattedContext, Interaction, MemoryId, MemoryItem, RetrieveQuery,
    };
    use std::sync::Arc;
    use tokio::sync::Mutex;

    struct DispatchStub {
        committed: Arc<Mutex<Vec<String>>>,
    }
    #[async_trait::async_trait]
    impl fm_core::FusionMemoryEngine for DispatchStub {
        async fn commit_episodic_memory(
            &self,
            _s: &str,
            ix: &Interaction,
        ) -> fm_core::MemoryResult<Vec<MemoryId>> {
            let mut g = self.committed.lock().await;
            g.push(ix.id.clone());
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
                total_tokens: 7,
            })
        }
        async fn consolidate_memories(&self) -> fm_core::MemoryResult<ConsolidationReport> {
            Ok(ConsolidationReport {
                dropped: 1,
                ..Default::default()
            })
        }
        async fn get_memory(&self, id: &str) -> fm_core::MemoryResult<Option<MemoryItem>> {
            if id == "m0" {
                Ok(Some(MemoryItem::new_turn_skeleton(
                    "m0".into(),
                    "ix0".into(),
                    0,
                    "s".into(),
                    fm_core::MemoryType::Episodic,
                    "content".into(),
                    1,
                )))
            } else {
                Ok(None)
            }
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

    fn stub_handle() -> (EngineHandle, Arc<Mutex<Vec<String>>>) {
        let committed = Arc::new(Mutex::new(vec![]));
        (
            EngineHandle::from_concrete(DispatchStub {
                committed: committed.clone(),
            }),
            committed,
        )
    }

    fn rpc(method: &str, params: serde_json::Value, id: i64) -> RpcRequest {
        RpcRequest {
            jsonrpc: "2.0".into(),
            method: method.into(),
            params,
            id: Value::from(id),
        }
    }

    async fn dispatch_ok(req: &RpcRequest, eng: &EngineHandle) -> Value {
        let resp = dispatch(req, eng).await;
        assert!(resp.error.is_none(), "unexpected error: {:?}", resp.error);
        resp.result.expect("missing result")
    }

    async fn dispatch_err(req: &RpcRequest, eng: &EngineHandle) -> RpcError {
        let resp = dispatch(req, eng).await;
        resp.error.expect("expected error, got result")
    }

    #[tokio::test]
    async fn dispatch_health() {
        let (eng, _) = stub_handle();
        let v = dispatch_ok(&rpc("health", serde_json::json!({}), 1), &eng).await;
        assert_eq!(v, Value::String("ok".into()));
    }

    #[tokio::test]
    async fn dispatch_commit() {
        let (eng, committed) = stub_handle();
        let params = serde_json::json!({
            "session_id":"s",
            "interaction":{"id":"ix1","session_id":"s","turns":[{"turn_idx":0,"user_message":"hi","assistant_message":"yo","tool_calls":[]}],"timestamp":1,"metadata":{}}
        });
        let v = dispatch_ok(&rpc("commit", params, 2), &eng).await;
        let ids: Vec<String> = serde_json::from_value(v).unwrap();
        assert_eq!(ids, vec!["m0".to_string()]);
        assert!(committed.lock().await.contains(&"ix1".to_string()));
    }

    #[tokio::test]
    async fn dispatch_retrieve() {
        let (eng, _) = stub_handle();
        let v = dispatch_ok(&rpc("retrieve", serde_json::json!({"text":"hi"}), 3), &eng).await;
        let ctx: FormattedContext = serde_json::from_value(v).unwrap();
        assert_eq!(ctx.total_tokens, 7);
    }

    #[tokio::test]
    async fn dispatch_consolidate() {
        let (eng, _) = stub_handle();
        let v = dispatch_ok(&rpc("consolidate", serde_json::json!({}), 4), &eng).await;
        let report: ConsolidationReport = serde_json::from_value(v).unwrap();
        assert_eq!(report.dropped, 1);
    }

    #[tokio::test]
    async fn dispatch_get_hit() {
        let (eng, _) = stub_handle();
        let v = dispatch_ok(&rpc("get", serde_json::json!({"id":"m0"}), 5), &eng).await;
        let m: MemoryItem = serde_json::from_value(v).unwrap();
        assert_eq!(m.id, "m0");
    }

    #[tokio::test]
    async fn dispatch_get_miss() {
        let (eng, _) = stub_handle();
        let v = dispatch_ok(&rpc("get", serde_json::json!({"id":"none"}), 6), &eng).await;
        assert!(v.is_null());
    }

    #[tokio::test]
    async fn dispatch_delete_with_confirm() {
        let (eng, _) = stub_handle();
        let v = dispatch_ok(
            &rpc("delete", serde_json::json!({"id":"m0","confirm":true}), 7),
            &eng,
        )
        .await;
        assert_eq!(v, Value::String("deleted".into()));
    }

    #[tokio::test]
    async fn dispatch_delete_without_confirm_rejected() {
        let (eng, _) = stub_handle();
        let e = dispatch_err(&rpc("delete", serde_json::json!({"id":"m0"}), 8), &eng).await;
        assert_eq!(e.code, -32602);
    }

    #[tokio::test]
    async fn dispatch_audit() {
        let (eng, _) = stub_handle();
        let v = dispatch_ok(
            &rpc("audit", serde_json::json!({"entity_ids":["e1"]}), 9),
            &eng,
        )
        .await;
        let ms: Vec<MemoryItem> = serde_json::from_value(v).unwrap();
        assert!(ms.is_empty());
    }

    #[tokio::test]
    async fn dispatch_method_not_found() {
        let (eng, _) = stub_handle();
        let e = dispatch_err(&rpc("frobnicate", serde_json::json!({}), 10), &eng).await;
        assert_eq!(e.code, -32601);
    }

    #[tokio::test]
    async fn dispatch_commit_bad_params() {
        let (eng, _) = stub_handle();
        let e = dispatch_err(&rpc("commit", serde_json::json!({"bad":1}), 11), &eng).await;
        assert_eq!(e.code, -32602);
    }

    #[test]
    fn parse_line_rejects_garbage() {
        assert!(parse_line("not json").is_none());
        assert!(parse_line("").is_none());
    }
}
