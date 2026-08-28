//! JSON-RPC 2.0 共用 dispatch。UDS + HTTP 复用。PRD §11.2。
//!
//! 方法: commit/retrieve/consolidate/get/delete/audit/health/version
//!      + memory.retrieve_context (issue #1/#4 fusion-event 契约)
//!      + delete_scope/count (issue #2 fusion-agent-studio adapter 契约)。
//! delete/delete_scope 需 params.confirm=true（B5 二次确认）。
//! P2-4: API 版本控制。jsonrpc=="2.0" 校验 (非 2.0 → -32600); 方法版本前缀 v1.<method>
//!       路由 (无前缀 = 最新 = v1); version 方法返支持的 api_version 供客户端协商。

use axum::http::StatusCode;
use fm_core::{ConsolidationReport, FormattedContext, Interaction, MemoryItem, RetrieveQuery};
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
    // P2-4: invalid_request → -32600 (JSON-RPC spec)。jsonrpc 字段非 "2.0" 用此。
    pub fn invalid_request(msg: impl Into<String>) -> Self {
        Self {
            code: -32600,
            message: msg.into(),
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
    // §3.1: NotFound → -32001 (server error 段, 永久)。客户端 fail-fast, 不重试。
    pub fn not_found(msg: impl Into<String>) -> Self {
        Self {
            code: -32001,
            message: msg.into(),
        }
    }
    // §2.8: Poisoned → -32002 (永久, 需重启)。运维据此识别锁中毒。
    pub fn poisoned(msg: impl Into<String>) -> Self {
        Self {
            code: -32002,
            message: msg.into(),
        }
    }
    // §2.8/§3.1: Busy → -32003 (瞬时, 可重试)。客户端退避后重试。
    pub fn busy(msg: impl Into<String>) -> Self {
        Self {
            code: -32003,
            message: msg.into(),
        }
    }
    // P1-5: Unauthorized → -32004 (UDS token 不匹配, 连接级)。多租户 UDS 鉴权。
    pub fn unauthorized() -> Self {
        Self {
            code: -32004,
            message: "unauthorized".into(),
        }
    }

    // §3.1: 按 MemoryError 分类返回码。NotFound→-32001, Poisoned→-32002, Busy→-32003, 其余→-32603。
    // 旧版全压 -32603 internal, 客户端无法区分 "引擎 bug"/"id 不存在"/"瞬时 busy"/"锁中毒"。
    pub fn from_engine(e: &fm_core::MemoryError) -> Self {
        if e.is_not_found() {
            Self::not_found(e.to_string())
        } else if matches!(e, fm_core::MemoryError::Poisoned) {
            Self::poisoned(e.to_string())
        } else if e.retryable() {
            Self::busy(e.to_string())
        } else {
            Self::internal(e.to_string())
        }
    }
}

/// P2-4: 当前 API 版本。方法前缀 v1.<method> 显式钉版本; 无前缀 = 最新 = v1。
/// version 方法返此值供客户端协商。升级破坏性变更时 bump, 老 vN 仍路由保向后兼容。
pub const API_VERSION: u32 = 1;

/// §3.8: dispatch 取 owned RpcRequest, 各 handler 取 owned params, 消除 `req.params.clone()` 深克隆。
/// 旧版 8 handler 全 `serde_json::from_value(req.params.clone())` — 已反序列化的 params 再深克隆整树再解析。
/// id 仍 clone 进响应 (Value 多为小整数/字符串, 克隆廉价, 响应需保留 id 不可避免)。
pub async fn dispatch(req: RpcRequest, engine: &EngineHandle) -> RpcResponse {
    // P2-4: jsonrpc 版本校验。非 "2.0" → -32600 invalid_request (spec), 旧版静默吞 (字段 _ 丢弃)。
    if req.jsonrpc != "2.0" {
        warn!(jsonrpc = %req.jsonrpc, "rpc rejected: jsonrpc field not 2.0");
        return RpcResponse {
            jsonrpc: "2.0".into(),
            result: None,
            error: Some(RpcError::invalid_request(format!(
                "jsonrpc must be \"2.0\", got {:?}",
                req.jsonrpc
            ))),
            id: req.id,
        };
    }
    // 拆字段: method 仅借用做匹配, params move 进 handler, id move 进响应。
    let RpcRequest {
        method,
        params,
        id,
        jsonrpc: _,
    } = req;
    // P2-4: 方法版本前缀路由。v1.<method> 显式钉版本 (校验 == 当前版), 无前缀 = 最新 = v1。
    // memory.<x> 命名空间 (issue 契约) 不以 v 开头, 不受影响。
    let method = match method.strip_prefix("v1.") {
        Some(rest) => rest.to_string(),
        None => method,
    };
    debug!(method = %method, "rpc dispatch");
    let res: Result<Value, RpcError> = match method.as_str() {
        "commit" => commit(params, engine).await,
        "retrieve" => retrieve(params, engine).await,
        // issue #1/#4: fusion-event 契约别名 (text-in, fused shape out)。
        "memory.retrieve_context" => retrieve_context_contract(params, engine).await,
        "consolidate" => consolidate(engine).await,
        "get" => get(params, engine).await,
        "delete" => delete(params, engine).await,
        // issue #2: fusion-agent-studio adapter 契约 (scope 批量删 + 计数)。
        "delete_scope" => delete_scope(params, engine).await,
        "count" => count(params, engine).await,
        "audit" => audit(params, engine).await,
        "health" => Ok(Value::String("ok".into())),
        // P2-4: version 方法 — 返当前 api_version 供客户端协商。
        "version" => Ok(serde_json::json!({"api_version": API_VERSION})),
        other => Err(RpcError::method_not_found(other)),
    };
    match res {
        Ok(v) => RpcResponse {
            jsonrpc: "2.0".into(),
            result: Some(v),
            error: None,
            id,
        },
        Err(e) => {
            warn!(code = e.code, msg = %e.message, "rpc error");
            RpcResponse {
                jsonrpc: "2.0".into(),
                result: None,
                error: Some(e),
                id,
            }
        }
    }
}

/// §3.18: 序列化 RpcResponse 为 JSON 行, 永不发畸形 `{}`。
/// 旧版 uds `to_string(&resp).unwrap_or_else(|_| "{}".into())` — 序列化失败客户端收字面 `{}`,
/// 无 jsonrpc/id/error 字段, 客户端解析器失配/挂起、不知哪个 request id 失败。
/// 改: 主序列化失败 → 降级为合法 jsonrpc 2.0 error 帧 (id=null, code=-32603); 该帧再失败 → 字面量兜底。
pub fn serialize_response(resp: &RpcResponse) -> String {
    serde_json::to_string(resp).unwrap_or_else(|e| {
        warn!(%e, "rpc response serialize failed, emitting error frame");
        serde_json::to_string(&RpcResponse {
            jsonrpc: "2.0".into(),
            result: None,
            error: Some(RpcError::internal(format!("serialize failed: {e}"))),
            id: Value::Null,
        })
        .unwrap_or_else(|_| {
            r#"{"jsonrpc":"2.0","result":null,"error":{"code":-32603,"message":"serialize failed"},"id":null}"#
                .into()
        })
    })
}

/// §2.9: RPC 错误码 → HTTP 状态码映射。引擎侧错误不再埋进 200 body。
/// -32700/-32601/-32602 (parse/method/params) → 400; -32001 NotFound → 404;
/// -32002 Poisoned/-32003 Busy → 503; -32603 internal → 500。
pub fn http_status_for_error(code: i64) -> StatusCode {
    match code {
        -32700 | -32600 | -32601 | -32602 => StatusCode::BAD_REQUEST,
        -32001 => StatusCode::NOT_FOUND,
        -32002 | -32003 => StatusCode::SERVICE_UNAVAILABLE,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

/// commit params。
#[derive(Debug, Deserialize)]
struct CommitParams {
    session_id: String,
    interaction: Interaction,
}

async fn commit(params: Value, engine: &EngineHandle) -> Result<Value, RpcError> {
    let p: CommitParams =
        serde_json::from_value(params).map_err(|e| RpcError::invalid_params(e.to_string()))?;
    // P1-1: 返回详细结果 (memory_ids + failed_turns), 客户端可感知失败 turn 并重试。
    // 旧契约 result: ["id1","id2"] (纯数组) → 改 result: {"memory_ids":[...],"failed_turns":[...]}。
    // 消费方契约测试仅断言 contains "result"/[/",故宽松断言不破; 新增字段 failed_turns。
    let outcome: fm_core::CommitOutcome = engine
        .commit_episodic_memory_detailed(&p.session_id, &p.interaction)
        .await
        .map_err(|e| RpcError::from_engine(&e))?;
    serde_json::to_value(&outcome).map_err(|e| RpcError::internal(e.to_string()))
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

async fn retrieve(params: Value, engine: &EngineHandle) -> Result<Value, RpcError> {
    let p: RetrieveParams =
        serde_json::from_value(params).map_err(|e| RpcError::invalid_params(e.to_string()))?;
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
        .map_err(|e| RpcError::from_engine(&e))?;
    serde_json::to_value(ctx).map_err(|e| RpcError::internal(e.to_string()))
}

async fn consolidate(engine: &EngineHandle) -> Result<Value, RpcError> {
    let report: ConsolidationReport = engine
        .consolidate_memories()
        .await
        .map_err(|e| RpcError::from_engine(&e))?;
    serde_json::to_value(report).map_err(|e| RpcError::internal(e.to_string()))
}

/// get params。
#[derive(Debug, Deserialize)]
struct GetParams {
    id: String,
}

async fn get(params: Value, engine: &EngineHandle) -> Result<Value, RpcError> {
    let p: GetParams =
        serde_json::from_value(params).map_err(|e| RpcError::invalid_params(e.to_string()))?;
    let m: Option<MemoryItem> = engine
        .get_memory(&p.id)
        .await
        .map_err(|e| RpcError::from_engine(&e))?;
    serde_json::to_value(m).map_err(|e| RpcError::internal(e.to_string()))
}

/// delete params（confirm 必填 true，B5）。
#[derive(Debug, Deserialize)]
struct DeleteParams {
    id: String,
    #[serde(default)]
    confirm: bool,
}

async fn delete(params: Value, engine: &EngineHandle) -> Result<Value, RpcError> {
    let p: DeleteParams =
        serde_json::from_value(params).map_err(|e| RpcError::invalid_params(e.to_string()))?;
    if !p.confirm {
        return Err(RpcError::invalid_params(
            "delete requires confirm=true (B5 二次确认)",
        ));
    }
    engine
        .delete_memory(&p.id)
        .await
        .map_err(|e| RpcError::from_engine(&e))?;
    Ok(Value::String("deleted".into()))
}

/// audit params。
#[derive(Debug, Deserialize)]
struct AuditParams {
    entity_ids: Vec<String>,
}

async fn audit(params: Value, engine: &EngineHandle) -> Result<Value, RpcError> {
    let p: AuditParams =
        serde_json::from_value(params).map_err(|e| RpcError::invalid_params(e.to_string()))?;
    let ms: Vec<MemoryItem> = engine
        .audit_memory_access(&p.entity_ids)
        .await
        .map_err(|e| RpcError::from_engine(&e))?;
    serde_json::to_value(ms).map_err(|e| RpcError::internal(e.to_string()))
}

/// delete_scope params (issue #2)。scope = session_id。confirm 必填 true (B5)。
#[derive(Debug, Deserialize)]
struct DeleteScopeParams {
    scope: String,
    #[serde(default)]
    confirm: bool,
}

async fn delete_scope(params: Value, engine: &EngineHandle) -> Result<Value, RpcError> {
    let p: DeleteScopeParams =
        serde_json::from_value(params).map_err(|e| RpcError::invalid_params(e.to_string()))?;
    if !p.confirm {
        return Err(RpcError::invalid_params(
            "delete_scope requires confirm=true (B5 二次确认)",
        ));
    }
    let n = engine
        .delete_scope(&p.scope)
        .await
        .map_err(|e| RpcError::from_engine(&e))?;
    serde_json::to_value(serde_json::json!({"deleted_count": n}))
        .map_err(|e| RpcError::internal(e.to_string()))
}

/// count params (issue #2)。scope 可选 (None → 全量)。
#[derive(Debug, Deserialize)]
struct CountParams {
    #[serde(default)]
    scope: Option<String>,
}

async fn count(params: Value, engine: &EngineHandle) -> Result<Value, RpcError> {
    let p: CountParams =
        serde_json::from_value(params).map_err(|e| RpcError::invalid_params(e.to_string()))?;
    let n = engine
        .count(p.scope.as_deref())
        .await
        .map_err(|e| RpcError::from_engine(&e))?;
    serde_json::to_value(serde_json::json!({"count": n}))
        .map_err(|e| RpcError::internal(e.to_string()))
}

/// memory.retrieve_context params (issue #1/#4, fusion-event 冻结契约)。
/// query 为文本串 (target_path|event_type), memory 内部 embed; trigger_id/node_id 仅透传留痕,
/// 不影响检索结果 (node_id 多节点集群留待 M6+ 扩展, 当前本地全检索)。
#[derive(Debug, Deserialize)]
struct RetrieveContextContractParams {
    #[serde(default)]
    trigger_id: String,
    query: String,
    #[serde(default = "default_top_k")]
    top_k: usize,
    #[serde(default)]
    node_id: String,
}

/// 契约返回: {context, memory_ids, cache_hit}。context = block turns_text 拼接;
/// memory_ids = 命中 interaction_id 去重列表; cache_hit 恒 false (memory 端不缓存, 调用方自管 TTL)。
async fn retrieve_context_contract(
    params: Value,
    engine: &EngineHandle,
) -> Result<Value, RpcError> {
    let p: RetrieveContextContractParams =
        serde_json::from_value(params).map_err(|e| RpcError::invalid_params(e.to_string()))?;
    debug!(trigger = %p.trigger_id, node = %p.node_id, "memory.retrieve_context contract");
    let q = RetrieveQuery {
        text: p.query,
        top_k: p.top_k,
        session_id: None,
        tier_filter: None,
        token_budget: default_budget(),
        aggregate: true,
    };
    let ctx: FormattedContext = engine
        .retrieve_context(&q)
        .await
        .map_err(|e| RpcError::from_engine(&e))?;
    let context = ctx
        .blocks
        .iter()
        .map(|b| b.turns_text.as_str())
        .collect::<Vec<_>>()
        .join("\n---\n");
    // §3.13: 旧版 `Vec::contains` O(n)/次 → O(n²) 块数 (100 块 = 1 万次串比较)。改 HashSet 去重 O(1)。
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut memory_ids: Vec<String> = Vec::new();
    for b in &ctx.blocks {
        if seen.insert(b.interaction_id.clone()) {
            memory_ids.push(b.interaction_id.clone());
        }
    }
    Ok(serde_json::json!({
        "context": context,
        "memory_ids": memory_ids,
        "cache_hit": false,
    }))
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
        // §3.1 新增分类码
        assert_eq!(RpcError::not_found("e").code, -32001);
        assert_eq!(RpcError::poisoned("e").code, -32002);
        assert_eq!(RpcError::busy("e").code, -32003);
    }

    // §3.1: from_engine 按 MemoryError 分类返回码。
    #[test]
    fn from_engine_classifies() {
        assert_eq!(
            RpcError::from_engine(&fm_core::MemoryError::NotFound("x".into())).code,
            -32001
        );
        assert_eq!(
            RpcError::from_engine(&fm_core::MemoryError::Poisoned).code,
            -32002
        );
        assert_eq!(
            RpcError::from_engine(&fm_core::MemoryError::Busy("x".into())).code,
            -32003
        );
        assert_eq!(
            RpcError::from_engine(&fm_core::MemoryError::Sqlite("boom".into())).code,
            -32603
        );
    }

    // ---- dispatch 全方法覆盖（StubEngine，不连 mlx）----
    // §1.12: 本组为 dispatch 路由接线测试 (StubEngine 返常量, 证 "wire passes params"),
    // 非行为测试。真实 MemoryEngine 经 HTTP/JSON-RPC 的行为覆盖见
    // tests/offline_integration.rs + tests/consumer_scenarios.rs (stub engine, 真栈往返)。

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
                stale_read: false,
                last_sync_at: 0,
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
        async fn delete_scope(&self, scope: &str) -> fm_core::MemoryResult<u64> {
            if scope == "sess-A" {
                Ok(3)
            } else {
                Ok(0)
            }
        }
        async fn count(&self, _scope: Option<&str>) -> fm_core::MemoryResult<u64> {
            Ok(42)
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
        let resp = dispatch(req.clone(), eng).await;
        assert!(resp.error.is_none(), "unexpected error: {:?}", resp.error);
        resp.result.expect("missing result")
    }

    async fn dispatch_err(req: &RpcRequest, eng: &EngineHandle) -> RpcError {
        let resp = dispatch(req.clone(), eng).await;
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
        // P1-1: commit 返回 CommitOutcome 对象 {memory_ids, failed_turns}, 非纯 id 数组。
        let outcome: fm_core::CommitOutcome = serde_json::from_value(v).unwrap();
        assert_eq!(
            outcome
                .memory_ids
                .iter()
                .map(|i| i.0.clone())
                .collect::<Vec<_>>(),
            vec!["m0".to_string()]
        );
        assert!(outcome.failed_turns.is_empty(), "no turn should fail");
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
    async fn dispatch_delete_scope_with_confirm() {
        let (eng, _) = stub_handle();
        let v = dispatch_ok(
            &rpc(
                "delete_scope",
                serde_json::json!({"scope":"sess-A","confirm":true}),
                12,
            ),
            &eng,
        )
        .await;
        assert_eq!(v["deleted_count"], 3);
    }

    #[tokio::test]
    async fn dispatch_delete_scope_without_confirm_rejected() {
        let (eng, _) = stub_handle();
        let e = dispatch_err(
            &rpc("delete_scope", serde_json::json!({"scope":"sess-A"}), 13),
            &eng,
        )
        .await;
        assert_eq!(e.code, -32602);
    }

    #[tokio::test]
    async fn dispatch_count() {
        let (eng, _) = stub_handle();
        let v = dispatch_ok(&rpc("count", serde_json::json!({}), 14), &eng).await;
        assert_eq!(v["count"], 42);
    }

    #[tokio::test]
    async fn dispatch_count_with_scope() {
        let (eng, _) = stub_handle();
        let v = dispatch_ok(
            &rpc("count", serde_json::json!({"scope":"sess-A"}), 15),
            &eng,
        )
        .await;
        assert_eq!(v["count"], 42);
    }

    #[tokio::test]
    async fn dispatch_retrieve_context_contract() {
        // issue #1/#4 契约: memory.retrieve_context → {context, memory_ids, cache_hit}
        let (eng, _) = stub_handle();
        let v = dispatch_ok(
            &rpc(
                "memory.retrieve_context",
                serde_json::json!({"trigger_id":"t1","query":"/a.swift|fileModified","top_k":5,"node_id":"macbook"}),
                16,
            ),
            &eng,
        )
        .await;
        assert!(v["context"].is_string(), "context 应为 string, got {v}");
        assert!(v["memory_ids"].is_array(), "memory_ids 应为 array");
        assert_eq!(v["cache_hit"], false);
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

    // ---- P2-4: API 版本控制 ----

    fn rpc_with_version(v: &str, method: &str, params: serde_json::Value, id: i64) -> RpcRequest {
        RpcRequest {
            jsonrpc: v.into(),
            method: method.into(),
            params,
            id: Value::from(id),
        }
    }

    #[tokio::test]
    async fn p2_4_rejects_non_2_0_jsonrpc() {
        // jsonrpc != "2.0" → -32600 invalid_request (旧版静默吞, 字段丢弃)
        let (eng, _) = stub_handle();
        let resp = dispatch(
            rpc_with_version("1.0", "health", serde_json::json!({}), 1),
            &eng,
        )
        .await;
        let err = resp.error.expect("non-2.0 应被拒");
        assert_eq!(err.code, -32600, "jsonrpc 非 2.0 → invalid_request");
        assert!(err.message.contains("2.0"), "错误信息须指明需 2.0");
    }

    #[tokio::test]
    async fn p2_4_v1_prefix_routes_to_handler() {
        // v1.health 显式钉版本 → 路由到 health (校验 == 当前版, 转发)
        let (eng, _) = stub_handle();
        let resp = dispatch(
            rpc_with_version("2.0", "v1.health", serde_json::json!({}), 2),
            &eng,
        )
        .await;
        assert!(resp.error.is_none(), "v1. 前缀应路由成功");
        assert_eq!(resp.result, Some(Value::String("ok".into())));
    }

    #[tokio::test]
    async fn p2_4_version_method_returns_api_version() {
        // version 方法 → {api_version: 1} 供客户端协商
        let (eng, _) = stub_handle();
        let v = dispatch_ok(&rpc("version", serde_json::json!({}), 3), &eng).await;
        assert_eq!(v["api_version"], API_VERSION);
    }

    #[tokio::test]
    async fn p2_4_bare_method_routes_as_latest() {
        // 无前缀 = 最新 = v1 (向后兼容: 现有客户端无前缀调用不受影响)
        let (eng, _) = stub_handle();
        let v = dispatch_ok(&rpc("health", serde_json::json!({}), 4), &eng).await;
        assert_eq!(v, Value::String("ok".into()));
    }

    #[test]
    fn p2_4_invalid_request_http_status() {
        // -32600 invalid_request → 400 BAD_REQUEST (http_status_for_error 映射)
        use axum::http::StatusCode;
        assert_eq!(http_status_for_error(-32600), StatusCode::BAD_REQUEST);
    }

    // §2.9: RPC 错误码 → HTTP 状态码映射。引擎错误不再埋进 200 body。
    #[test]
    fn http_status_mapping() {
        use axum::http::StatusCode;
        assert_eq!(http_status_for_error(-32700), StatusCode::BAD_REQUEST);
        assert_eq!(http_status_for_error(-32601), StatusCode::BAD_REQUEST);
        assert_eq!(http_status_for_error(-32602), StatusCode::BAD_REQUEST);
        assert_eq!(http_status_for_error(-32001), StatusCode::NOT_FOUND);
        assert_eq!(
            http_status_for_error(-32002),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            http_status_for_error(-32003),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            http_status_for_error(-32603),
            StatusCode::INTERNAL_SERVER_ERROR
        );
        assert_eq!(
            http_status_for_error(-99999),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    // §3.18: serialize_response 永不发畸形 `{}`, 序列化失败降级合法 jsonrpc error 帧。
    #[test]
    fn serialize_response_is_valid_jsonrpc() {
        let resp = RpcResponse {
            jsonrpc: "2.0".into(),
            result: Some(Value::String("ok".into())),
            error: None,
            id: Value::from(1i64),
        };
        let s = serialize_response(&resp);
        let v: Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["jsonrpc"], "2.0");
        assert_eq!(v["result"], "ok");
        assert_eq!(v["id"], 1);
    }

    // §3.13: retrieve_context_contract 去重保留首现顺序, HashSet O(1) 替 Vec::contains O(n)。
    #[tokio::test]
    async fn dispatch_retrieve_context_contract_dedup() {
        // 造 stub 返多块同 interaction_id, 验证 memory_ids 去重且顺序保留。
        struct DupStub;
        #[async_trait::async_trait]
        impl fm_core::FusionMemoryEngine for DupStub {
            async fn commit_episodic_memory(
                &self,
                _: &str,
                _: &Interaction,
            ) -> fm_core::MemoryResult<Vec<MemoryId>> {
                Ok(vec![])
            }
            async fn retrieve_context(
                &self,
                _: &RetrieveQuery,
            ) -> fm_core::MemoryResult<FormattedContext> {
                use fm_core::context::ContextBlock;
                use fm_core::MemoryType;
                let mk = |iid: &str, txt: &str| ContextBlock {
                    interaction_id: iid.into(),
                    turns: vec![],
                    memory_type: MemoryType::Episodic,
                    turns_text: txt.into(),
                    score: 0.0,
                    source_entities: vec![],
                };
                Ok(FormattedContext {
                    blocks: vec![mk("ixA", "t1"), mk("ixA", "t2"), mk("ixB", "t3")],
                    total_tokens: 3,
                    stale_read: false,
                    last_sync_at: 0,
                })
            }
            async fn consolidate_memories(&self) -> fm_core::MemoryResult<ConsolidationReport> {
                Ok(ConsolidationReport::default())
            }
            async fn get_memory(&self, _: &str) -> fm_core::MemoryResult<Option<MemoryItem>> {
                Ok(None)
            }
            async fn delete_memory(&self, _: &str) -> fm_core::MemoryResult<()> {
                Ok(())
            }
            async fn audit_memory_access(
                &self,
                _: &[String],
            ) -> fm_core::MemoryResult<Vec<MemoryItem>> {
                Ok(vec![])
            }
        }
        let eng = EngineHandle::from_concrete(DupStub);
        let v = dispatch_ok(
            &rpc(
                "memory.retrieve_context",
                serde_json::json!({"query":"q"}),
                17,
            ),
            &eng,
        )
        .await;
        let ids: Vec<String> = serde_json::from_value(v["memory_ids"].clone()).unwrap();
        assert_eq!(ids, vec!["ixA".to_string(), "ixB".to_string()]);
    }
}
