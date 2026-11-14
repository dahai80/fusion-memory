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
/// M5 #46: per-memory ACL 调用方身份 (multi_tenant+ACL 模式下由 HTTP 边界从 JWT 构造)。
/// UDS (本机可信) / 单租户 / ACL 关 → None = 不启用 owner 过滤 (旧行为)。
#[derive(Debug, Clone)]
pub struct AclUser {
    /// fusion-identity JWT sub (owner 位比对值)。
    pub sub: String,
    /// role == "admin" → 绕过 owner 检查。
    pub admin: bool,
}

/// M5 #46: owner 检查。deny 当且仅当调用方带 ACL 身份且记忆有 owner 且不匹配且非 admin。
/// owner None = 共享记忆 (所有人可见); 调用方 sub 空 (旧版 identity 无 sub claim) = fail-closed 全拒。
fn acl_denies(acl: Option<&AclUser>, owner: &Option<String>) -> bool {
    match (acl, owner) {
        (Some(u), Some(o)) => !u.admin && u.sub != *o,
        _ => false,
    }
}

pub async fn dispatch(
    req: RpcRequest,
    engine: &EngineHandle,
    tenant: &str,
    acl: Option<&AclUser>,
) -> RpcResponse {
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
        "commit" => commit(params, engine, tenant, acl).await,
        "retrieve" => retrieve(params, engine, tenant, acl).await,
        // issue #1/#4: fusion-event 契约别名 (text-in, fused shape out)。
        "memory.retrieve_context" => retrieve_context_contract(params, engine, tenant, acl).await,
        "consolidate" => consolidate(params, engine).await,
        "get" => get(params, engine, tenant, acl).await,
        "delete" => delete(params, engine, tenant, acl).await,
        // issue #2: fusion-agent-studio adapter 契约 (scope 批量删 + 计数)。
        "delete_scope" => delete_scope(params, engine, tenant).await,
        "count" => count(params, engine, tenant).await,
        "audit" => audit(params, engine).await,
        // #33 web surface: browse/manage methods (shared by the web UI /app and scripts).
        "list" => list(params, engine, tenant, acl).await,
        "audit_log" => audit_log(params, engine).await,
        "merges" => merges(engine).await,
        "unmerge" => unmerge(params, engine).await,
        "reconcile" => reconcile(params, engine).await,
        // M6 #45: poisoning-governance provenance query (lineage + reverse citations).
        "provenance" => provenance(params, engine, tenant, acl).await,
        // M8 #49: content revision history + standard export (JSONL / Markdown).
        "revisions" => revisions(params, engine, tenant, acl).await,
        "export" => export(params, engine, tenant, acl).await,
        // M7 #48: procedural memory -> SKILL.md export (agent-studio / fusion-harness).
        "export_skills" => export_skills(params, engine, tenant, acl).await,
        // v1.10.0 #57/#58: revoke-by-meta + share grants + playbook publish/adopt.
        "revoke" => revoke(params, engine, tenant).await,
        "grant_access" => grant_access(params, engine).await,
        "revoke_access" => revoke_access(params, engine).await,
        "list_grants" => list_grants(params, engine).await,
        "publish_playbook" => publish_playbook(params, engine, tenant).await,
        "adopt_playbook" => adopt_playbook(params, engine, tenant).await,
        // v1.9.0 #53: governance panel — read-only scoring/forgetting parameter snapshot.
        "config" => Ok(fm_engine::scoring::governance_snapshot()),
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

/// issue #21: namespace 参数校验 (user/project/local/空)。非法值 invalid_params, 不静默降级。
fn validate_namespace(ns: &str) -> Result<(), RpcError> {
    if fm_core::namespace::is_valid_namespace(ns) {
        Ok(())
    } else {
        Err(RpcError::invalid_params(format!(
            "namespace must be one of user|project|local|\"\", got {ns:?}"
        )))
    }
}

/// commit params。
#[derive(Debug, Deserialize)]
struct CommitParams {
    session_id: String,
    interaction: Interaction,
    /// issue #21: 显式作用域命名空间 (优先于 interaction 内值)。
    #[serde(default)]
    namespace: Option<String>,
    #[serde(default)]
    namespace_key: Option<String>,
}

async fn commit(
    params: Value,
    engine: &EngineHandle,
    tenant: &str,
    acl: Option<&AclUser>,
) -> Result<Value, RpcError> {
    let mut p: CommitParams =
        serde_json::from_value(params).map_err(|e| RpcError::invalid_params(e.to_string()))?;
    // #16: 权威租户取自请求 (gateway X-Fusion-Tenant), 覆盖 interaction 内任何客户端租户值。
    // 空 tenant = 默认租户 (单租户向后兼容)。
    if !tenant.is_empty() {
        p.interaction.tenant = tenant.to_string();
    }
    // #21: 显式 namespace 参数覆盖 interaction 内值 (与服务端权威参数模式一致)。
    if let Some(ns) = p.namespace.as_deref() {
        validate_namespace(ns)?;
        p.interaction.namespace = ns.to_string();
    }
    validate_namespace(&p.interaction.namespace)?;
    if let Some(key) = p.namespace_key {
        p.interaction.namespace_key = key;
    }
    // M5 #46: owner 权威来源 = 服务端 JWT sub (同 tenant 模式 — 客户端自报值覆盖丢弃)。
    if let Some(u) = acl {
        p.interaction.owner = Some(u.sub.clone());
    }
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
    /// issue #21: 检索作用域命名空间 (user/project/local), 空 = 不过滤。
    #[serde(default)]
    namespace: Option<String>,
    #[serde(default)]
    namespace_key: Option<String>,
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

async fn retrieve(
    params: Value,
    engine: &EngineHandle,
    tenant: &str,
    acl: Option<&AclUser>,
) -> Result<Value, RpcError> {
    let p: RetrieveParams =
        serde_json::from_value(params).map_err(|e| RpcError::invalid_params(e.to_string()))?;
    let namespace = p.namespace.unwrap_or_default();
    validate_namespace(&namespace)?;
    let q = RetrieveQuery {
        text: p.text,
        top_k: p.top_k,
        session_id: p.session_id,
        // #16: 权威租户作用域检索。 #21: namespace 作用域过滤。
        tenant: tenant.to_string(),
        namespace,
        namespace_key: p.namespace_key.unwrap_or_default(),
        tier_filter: None,
        token_budget: p.token_budget,
        aggregate: p.aggregate,
    };
    let mut ctx: FormattedContext = engine
        .retrieve_context(&q)
        .await
        .map_err(|e| RpcError::from_engine(&e))?;
    // M5 #46: 检索边界过滤 — 调用方不可见的 owner 块整体剔除 (post-filter, 引擎不感知身份)。
    ctx.blocks.retain(|b| !acl_denies(acl, &b.owner));
    serde_json::to_value(ctx).map_err(|e| RpcError::internal(e.to_string()))
}

/// consolidate params (issue #21: 可选 namespace 作用域调度)。
#[derive(Debug, Default, Deserialize)]
struct ConsolidateParams {
    #[serde(default)]
    namespace: Option<String>,
    #[serde(default)]
    namespace_key: Option<String>,
}

async fn consolidate(params: Value, engine: &EngineHandle) -> Result<Value, RpcError> {
    // params 可为 null/空对象 (旧调用方), serde 默认字段全空 → 全局调度。
    let p: ConsolidateParams = if params.is_null() {
        ConsolidateParams::default()
    } else {
        serde_json::from_value(params).map_err(|e| RpcError::invalid_params(e.to_string()))?
    };
    let namespace = p.namespace.unwrap_or_default();
    validate_namespace(&namespace)?;
    let report: ConsolidationReport = if namespace.is_empty() {
        engine
            .consolidate_memories()
            .await
            .map_err(|e| RpcError::from_engine(&e))?
    } else {
        engine
            .consolidate_memories_scoped(&namespace, p.namespace_key.as_deref().unwrap_or(""))
            .await
            .map_err(|e| RpcError::from_engine(&e))?
    };
    serde_json::to_value(report).map_err(|e| RpcError::internal(e.to_string()))
}

/// get params。
#[derive(Debug, Deserialize)]
struct GetParams {
    id: String,
}

async fn get(
    params: Value,
    engine: &EngineHandle,
    tenant: &str,
    acl: Option<&AclUser>,
) -> Result<Value, RpcError> {
    let p: GetParams =
        serde_json::from_value(params).map_err(|e| RpcError::invalid_params(e.to_string()))?;
    // #16: 按 tenant 取, 跨租户 → None。
    let m: Option<MemoryItem> = engine
        .get_memory_tenant(&p.id, tenant)
        .await
        .map_err(|e| RpcError::from_engine(&e))?;
    // M5 #46: owner 不匹配 → NotFound (不泄露存在性, fail-closed)。
    if let Some(item) = &m {
        if acl_denies(acl, &item.owner) {
            return Err(RpcError::not_found(&p.id));
        }
    }
    serde_json::to_value(m).map_err(|e| RpcError::internal(e.to_string()))
}

/// delete params（confirm 必填 true，B5）。
#[derive(Debug, Deserialize)]
struct DeleteParams {
    id: String,
    #[serde(default)]
    confirm: bool,
}

async fn delete(
    params: Value,
    engine: &EngineHandle,
    tenant: &str,
    acl: Option<&AclUser>,
) -> Result<Value, RpcError> {
    let p: DeleteParams =
        serde_json::from_value(params).map_err(|e| RpcError::invalid_params(e.to_string()))?;
    if !p.confirm {
        return Err(RpcError::invalid_params(
            "delete requires confirm=true (B5 二次确认)",
        ));
    }
    // M5 #46: owner 不匹配 → NotFound, 不落删除 (先读后删, fail-closed)。
    let existing: Option<MemoryItem> = engine
        .get_memory_tenant(&p.id, tenant)
        .await
        .map_err(|e| RpcError::from_engine(&e))?;
    if let Some(item) = &existing {
        if acl_denies(acl, &item.owner) {
            return Err(RpcError::not_found(&p.id));
        }
    }
    // #16: 按 tenant 删, 跨租户 → NotFound。
    engine
        .delete_memory_tenant(&p.id, tenant)
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

async fn delete_scope(
    params: Value,
    engine: &EngineHandle,
    tenant: &str,
) -> Result<Value, RpcError> {
    let p: DeleteScopeParams =
        serde_json::from_value(params).map_err(|e| RpcError::invalid_params(e.to_string()))?;
    if !p.confirm {
        return Err(RpcError::invalid_params(
            "delete_scope requires confirm=true (B5 二次确认)",
        ));
    }
    // #16: 按 tenant + scope 删, 跨租户同 session 不误删。
    let n = engine
        .delete_scope_tenant(&p.scope, tenant)
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

async fn count(params: Value, engine: &EngineHandle, tenant: &str) -> Result<Value, RpcError> {
    let p: CountParams =
        serde_json::from_value(params).map_err(|e| RpcError::invalid_params(e.to_string()))?;
    // #16: 按 tenant + scope 计数, 跨租户不可见。
    let n = engine
        .count_tenant(p.scope.as_deref(), tenant)
        .await
        .map_err(|e| RpcError::from_engine(&e))?;
    serde_json::to_value(serde_json::json!({"count": n}))
        .map_err(|e| RpcError::internal(e.to_string()))
}

// ── v1.10.0 #57/#58: revoke-by-meta + share grants + playbook publish/adopt. ──

/// revoke params (#57)。filter 至少一字段 (persist 层 fail-closed), confirm 必填 (B5)。
#[derive(Debug, Deserialize)]
struct RevokeParams {
    filter: fm_core::RevokeFilter,
    #[serde(default)]
    confirm: bool,
}

async fn revoke(params: Value, engine: &EngineHandle, tenant: &str) -> Result<Value, RpcError> {
    let p: RevokeParams =
        serde_json::from_value(params).map_err(|e| RpcError::invalid_params(e.to_string()))?;
    if !p.confirm {
        return Err(RpcError::invalid_params(
            "revoke requires confirm=true (B5 double-confirmation)",
        ));
    }
    let n = engine
        .revoke_by_meta(&p.filter, tenant)
        .await
        .map_err(|e| RpcError::from_engine(&e))?;
    Ok(serde_json::json!({"revoked_count": n}))
}

/// grant_access params (#58)。namespace 现仅 agent (fail-closed)。
#[derive(Debug, Deserialize)]
struct GrantAccessParams {
    grantor_key: String,
    grantee_key: String,
    #[serde(default = "default_agent_ns")]
    namespace: String,
}

fn default_agent_ns() -> String {
    fm_core::namespace::NS_AGENT.into()
}

fn validate_grant_keys(p: &GrantAccessParams) -> Result<(), RpcError> {
    if p.grantor_key.trim().is_empty() || p.grantee_key.trim().is_empty() {
        return Err(RpcError::invalid_params(
            "grantor_key/grantee_key must be non-empty",
        ));
    }
    if p.grantor_key.len() > 256 || p.grantee_key.len() > 256 {
        return Err(RpcError::invalid_params("grant keys must be <= 256 chars"));
    }
    if p.namespace != fm_core::namespace::NS_AGENT {
        return Err(RpcError::invalid_params(
            r#"namespace must be "agent" (only supported grant tier)"#,
        ));
    }
    Ok(())
}

async fn grant_access(params: Value, engine: &EngineHandle) -> Result<Value, RpcError> {
    let p: GrantAccessParams =
        serde_json::from_value(params).map_err(|e| RpcError::invalid_params(e.to_string()))?;
    validate_grant_keys(&p)?;
    let grant = fm_core::ShareGrant {
        grantor_key: p.grantor_key.trim().to_string(),
        grantee_key: p.grantee_key.trim().to_string(),
        namespace: p.namespace,
        created_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0),
    };
    engine
        .grant_access(&grant)
        .await
        .map_err(|e| RpcError::from_engine(&e))?;
    Ok(Value::String("granted".into()))
}

/// revoke_access params (#58)。confirm 必填 (B5)。
#[derive(Debug, Deserialize)]
struct RevokeAccessParams {
    grantor_key: String,
    grantee_key: String,
    #[serde(default = "default_agent_ns")]
    namespace: String,
    #[serde(default)]
    confirm: bool,
}

async fn revoke_access(params: Value, engine: &EngineHandle) -> Result<Value, RpcError> {
    let p: RevokeAccessParams =
        serde_json::from_value(params).map_err(|e| RpcError::invalid_params(e.to_string()))?;
    if !p.confirm {
        return Err(RpcError::invalid_params(
            "revoke_access requires confirm=true (B5 double-confirmation)",
        ));
    }
    let n = engine
        .revoke_access(&p.grantor_key, &p.grantee_key, &p.namespace)
        .await
        .map_err(|e| RpcError::from_engine(&e))?;
    Ok(serde_json::json!({"removed": n}))
}

/// list_grants params (#58)。
#[derive(Debug, Deserialize)]
struct ListGrantsParams {
    grantee_key: String,
    #[serde(default = "default_agent_ns")]
    namespace: String,
}

async fn list_grants(params: Value, engine: &EngineHandle) -> Result<Value, RpcError> {
    let p: ListGrantsParams =
        serde_json::from_value(params).map_err(|e| RpcError::invalid_params(e.to_string()))?;
    let grants = engine
        .list_grants(&p.grantee_key, &p.namespace)
        .await
        .map_err(|e| RpcError::from_engine(&e))?;
    serde_json::to_value(grants).map_err(|e| RpcError::internal(e.to_string()))
}

/// publish_playbook params (#58)。
#[derive(Debug, Deserialize)]
struct PublishPlaybookParams {
    memory_id: String,
    publisher_key: String,
}

async fn publish_playbook(
    params: Value,
    engine: &EngineHandle,
    tenant: &str,
) -> Result<Value, RpcError> {
    let p: PublishPlaybookParams =
        serde_json::from_value(params).map_err(|e| RpcError::invalid_params(e.to_string()))?;
    if p.publisher_key.trim().is_empty() {
        return Err(RpcError::invalid_params("publisher_key must be non-empty"));
    }
    let id = engine
        .publish_playbook(&p.memory_id, p.publisher_key.trim(), tenant)
        .await
        .map_err(|e| RpcError::from_engine(&e))?;
    Ok(serde_json::json!({"playbook_id": id}))
}

/// adopt_playbook params (#58)。采纳必过 #57 准入门 (近重复拒绝)。
#[derive(Debug, Deserialize)]
struct AdoptPlaybookParams {
    playbook_id: String,
    consumer_namespace: String,
    consumer_key: String,
}

async fn adopt_playbook(
    params: Value,
    engine: &EngineHandle,
    tenant: &str,
) -> Result<Value, RpcError> {
    let p: AdoptPlaybookParams =
        serde_json::from_value(params).map_err(|e| RpcError::invalid_params(e.to_string()))?;
    if p.consumer_key.trim().is_empty() {
        return Err(RpcError::invalid_params("consumer_key must be non-empty"));
    }
    if !fm_core::namespace::is_valid_namespace(&p.consumer_namespace) {
        return Err(RpcError::invalid_params(format!(
            "consumer_namespace must be one of user|project|local|agent|playbook|empty, got {0:?}",
            p.consumer_namespace
        )));
    }
    let adopted = engine
        .adopt_playbook(
            &p.playbook_id,
            &p.consumer_namespace,
            p.consumer_key.trim(),
            tenant,
        )
        .await
        .map_err(|e| RpcError::from_engine(&e))?;
    match adopted {
        Some(id) => Ok(serde_json::json!({"adopted": true, "memory_id": id})),
        None => Ok(
            serde_json::json!({"adopted": false, "memory_id": Value::Null,
            "reason": "admission gate rejected (near-duplicate)"}),
        ),
    }
}

// ── #33 web surface (v1.5.0): browse/manage methods (list/audit_log/merges/unmerge/reconcile). ──
// Pure browse/ops surface, consumed by the web UI (served at /app) and scripts. Writes (unmerge/reconcile) follow B5
// confirm double-confirmation; reads scope tenant/namespace with the same semantics as retrieval.

/// list params (web memory browser pagination). limit hard-capped at 200 (bounded response body, DoS guard).
#[derive(Debug, Deserialize)]
struct ListParams {
    #[serde(default)]
    offset: usize,
    #[serde(default = "default_list_limit")]
    limit: usize,
    #[serde(default)]
    memory_type: Option<String>,
    #[serde(default)]
    namespace: String,
    #[serde(default)]
    namespace_key: String,
    #[serde(default)]
    include_tombstoned: bool,
}

fn default_list_limit() -> usize {
    50
}

async fn list(
    params: Value,
    engine: &EngineHandle,
    tenant: &str,
    acl: Option<&AclUser>,
) -> Result<Value, RpcError> {
    let p: ListParams =
        serde_json::from_value(params).map_err(|e| RpcError::invalid_params(e.to_string()))?;
    if p.limit == 0 || p.limit > 200 {
        return Err(RpcError::invalid_params("limit must be 1..=200"));
    }
    if let Some(mt) = &p.memory_type {
        if !matches!(mt.as_str(), "Episodic" | "Semantic" | "Procedural") {
            return Err(RpcError::invalid_params(format!(
                "memory_type must be Episodic|Semantic|Procedural, got {mt:?}"
            )));
        }
    }
    validate_namespace(&p.namespace)?;
    let q = fm_core::ListQuery {
        offset: p.offset,
        limit: p.limit,
        memory_type: p.memory_type,
        namespace: p.namespace,
        namespace_key: p.namespace_key,
        include_tombstoned: p.include_tombstoned,
    };
    let mut items = engine
        .list_memories(&q, tenant)
        .await
        .map_err(|e| RpcError::from_engine(&e))?;
    // M5 #46: 浏览边界同 owner 过滤 (可见条目少于 limit — 页内过滤语义, v1 接受)。
    items.retain(|m| !acl_denies(acl, &m.owner));
    serde_json::to_value(items).map_err(|e| RpcError::internal(e.to_string()))
}

/// audit_log params (web audit view). limit hard-capped at 500.
#[derive(Debug, Deserialize)]
struct AuditLogParams {
    #[serde(default = "default_audit_log_limit")]
    limit: usize,
}

fn default_audit_log_limit() -> usize {
    100
}

async fn audit_log(params: Value, engine: &EngineHandle) -> Result<Value, RpcError> {
    let p: AuditLogParams =
        serde_json::from_value(params).map_err(|e| RpcError::invalid_params(e.to_string()))?;
    if p.limit == 0 || p.limit > 500 {
        return Err(RpcError::invalid_params("limit must be 1..=500"));
    }
    let rows = engine
        .list_audit_log(p.limit)
        .await
        .map_err(|e| RpcError::from_engine(&e))?;
    serde_json::to_value(rows).map_err(|e| RpcError::internal(e.to_string()))
}

/// merges: list the full merge_log. No params ({} / null both fine).
async fn merges(engine: &EngineHandle) -> Result<Value, RpcError> {
    let rows = engine
        .list_merges()
        .await
        .map_err(|e| RpcError::from_engine(&e))?;
    serde_json::to_value(rows).map_err(|e| RpcError::internal(e.to_string()))
}

/// unmerge params. confirm must be true (B5; reverts the merge, restoring the source).
#[derive(Debug, Deserialize)]
struct UnmergeParams {
    id: u64,
    #[serde(default)]
    confirm: bool,
}

async fn unmerge(params: Value, engine: &EngineHandle) -> Result<Value, RpcError> {
    let p: UnmergeParams =
        serde_json::from_value(params).map_err(|e| RpcError::invalid_params(e.to_string()))?;
    if !p.confirm {
        return Err(RpcError::invalid_params(
            "unmerge requires confirm=true (B5 double-confirmation)",
        ));
    }
    let existed = engine
        .unmerge(p.id)
        .await
        .map_err(|e| RpcError::from_engine(&e))?;
    serde_json::to_value(serde_json::json!({"unmerged": existed}))
        .map_err(|e| RpcError::internal(e.to_string()))
}

/// reconcile params. confirm must be true (B5; cross-store reconciliation includes tombstone physical delete).
#[derive(Debug, Deserialize)]
struct ReconcileParams {
    #[serde(default)]
    confirm: bool,
}

async fn reconcile(params: Value, engine: &EngineHandle) -> Result<Value, RpcError> {
    let p: ReconcileParams =
        serde_json::from_value(params).map_err(|e| RpcError::invalid_params(e.to_string()))?;
    if !p.confirm {
        return Err(RpcError::invalid_params(
            "reconcile requires confirm=true (B5 double-confirmation)",
        ));
    }
    let n = engine
        .reconcile()
        .await
        .map_err(|e| RpcError::from_engine(&e))?;
    serde_json::to_value(serde_json::json!({"reconciled": n}))
        .map_err(|e| RpcError::internal(e.to_string()))
}

/// M6 #45: provenance params — one memory id; returns lineage + reverse citations.
#[derive(Debug, Deserialize)]
struct ProvenanceParams {
    id: String,
}

async fn provenance(
    params: Value,
    engine: &EngineHandle,
    tenant: &str,
    acl: Option<&AclUser>,
) -> Result<Value, RpcError> {
    let p: ProvenanceParams =
        serde_json::from_value(params).map_err(|e| RpcError::invalid_params(e.to_string()))?;
    if p.id.trim().is_empty() {
        return Err(RpcError::invalid_params(
            "provenance requires a non-empty id",
        ));
    }
    // M5 #46: 主体记忆 owner 不匹配 → NotFound (溯源不泄露他人记忆)。
    let subject: Option<MemoryItem> = engine
        .get_memory_tenant(&p.id, tenant)
        .await
        .map_err(|e| RpcError::from_engine(&e))?;
    if let Some(item) = &subject {
        if acl_denies(acl, &item.owner) {
            return Err(RpcError::not_found(&p.id));
        }
    }
    let rep = engine
        .provenance(&p.id, tenant)
        .await
        .map_err(|e| RpcError::from_engine(&e))?;
    serde_json::to_value(rep).map_err(|e| RpcError::internal(e.to_string()))
}

/// M8 #49: revisions params — one memory id; returns its content-revision history.
#[derive(Debug, Deserialize)]
struct RevisionsParams {
    id: String,
}

async fn revisions(
    params: Value,
    engine: &EngineHandle,
    tenant: &str,
    acl: Option<&AclUser>,
) -> Result<Value, RpcError> {
    let p: RevisionsParams =
        serde_json::from_value(params).map_err(|e| RpcError::invalid_params(e.to_string()))?;
    // ACL: someone else's memory → NotFound (history must not leak either).
    let subject: Option<MemoryItem> = engine
        .get_memory_tenant(&p.id, tenant)
        .await
        .map_err(|e| RpcError::from_engine(&e))?;
    if let Some(item) = &subject {
        if acl_denies(acl, &item.owner) {
            return Err(RpcError::not_found(&p.id));
        }
    } else {
        return Err(RpcError::not_found(&p.id));
    }
    let rows = engine
        .list_revisions(&p.id, tenant)
        .await
        .map_err(|e| RpcError::from_engine(&e))?;
    serde_json::to_value(rows).map_err(|e| RpcError::internal(e.to_string()))
}

/// M8 #49: export params. format = "jsonl" | "markdown"; optional namespace +
/// memory_type scoping; limit hard-capped (bounded response body, DoS guard).
#[derive(Debug, Deserialize)]
struct ExportParams {
    format: String,
    #[serde(default = "default_export_limit")]
    limit: usize,
    #[serde(default)]
    namespace: String,
    #[serde(default)]
    namespace_key: String,
    #[serde(default)]
    memory_type: Option<String>,
}

fn default_export_limit() -> usize {
    500
}

async fn export(
    params: Value,
    engine: &EngineHandle,
    tenant: &str,
    acl: Option<&AclUser>,
) -> Result<Value, RpcError> {
    let p: ExportParams =
        serde_json::from_value(params).map_err(|e| RpcError::invalid_params(e.to_string()))?;
    if !matches!(p.format.as_str(), "jsonl" | "markdown") {
        return Err(RpcError::invalid_params("format must be jsonl|markdown"));
    }
    if p.limit == 0 || p.limit > 1000 {
        return Err(RpcError::invalid_params("limit must be 1..=1000"));
    }
    if let Some(mt) = &p.memory_type {
        if !matches!(mt.as_str(), "Episodic" | "Semantic" | "Procedural") {
            return Err(RpcError::invalid_params(format!(
                "memory_type must be Episodic|Semantic|Procedural, got {mt:?}"
            )));
        }
    }
    validate_namespace(&p.namespace)?;
    let q = fm_core::ListQuery {
        offset: 0,
        limit: p.limit,
        memory_type: p.memory_type,
        namespace: p.namespace,
        namespace_key: p.namespace_key,
        include_tombstoned: false,
    };
    let mut items = engine
        .list_memories(&q, tenant)
        .await
        .map_err(|e| RpcError::from_engine(&e))?;
    // M5 #46: export respects the owner filter — other users' memories never leave.
    items.retain(|m| !acl_denies(acl, &m.owner));
    // M8 defense line 2: redact applies unconditionally inside the renderers.
    let body = if p.format == "jsonl" {
        fm_engine::export::export_jsonl(&items)
    } else {
        fm_engine::export::export_markdown(&items)
    };
    Ok(serde_json::json!({
        "format": p.format,
        "count": items.len(),
        "body": body,
    }))
}

/// M7 #48: export_skills params — Procedural memories rendered as SKILL.md docs.
#[derive(Debug, Deserialize)]
struct ExportSkillsParams {
    #[serde(default = "default_skills_limit")]
    limit: usize,
    #[serde(default)]
    namespace: String,
    #[serde(default)]
    namespace_key: String,
}

fn default_skills_limit() -> usize {
    20
}

async fn export_skills(
    params: Value,
    engine: &EngineHandle,
    tenant: &str,
    acl: Option<&AclUser>,
) -> Result<Value, RpcError> {
    let p: ExportSkillsParams =
        serde_json::from_value(params).map_err(|e| RpcError::invalid_params(e.to_string()))?;
    if p.limit == 0 || p.limit > 50 {
        return Err(RpcError::invalid_params("limit must be 1..=50"));
    }
    validate_namespace(&p.namespace)?;
    let q = fm_core::ListQuery {
        offset: 0,
        limit: p.limit,
        memory_type: Some("Procedural".into()),
        namespace: p.namespace,
        namespace_key: p.namespace_key,
        include_tombstoned: false,
    };
    let mut items = engine
        .list_memories(&q, tenant)
        .await
        .map_err(|e| RpcError::from_engine(&e))?;
    // M5 #46: skills built from someone else's memories must not leak either.
    items.retain(|m| !acl_denies(acl, &m.owner));
    let docs = fm_engine::export::render_skills(&items);
    serde_json::to_value(&docs).map_err(|e| RpcError::internal(e.to_string()))
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
    /// issue #21: 可选作用域过滤 (加法式, 旧调用方不传 = 不过滤)。
    #[serde(default)]
    namespace: Option<String>,
    #[serde(default)]
    namespace_key: Option<String>,
}

/// 契约返回: {context, memory_ids, cache_hit}。context = block turns_text 拼接;
/// memory_ids = 命中 interaction_id 去重列表; cache_hit 恒 false (memory 端不缓存, 调用方自管 TTL)。
async fn retrieve_context_contract(
    params: Value,
    engine: &EngineHandle,
    tenant: &str,
    acl: Option<&AclUser>,
) -> Result<Value, RpcError> {
    let p: RetrieveContextContractParams =
        serde_json::from_value(params).map_err(|e| RpcError::invalid_params(e.to_string()))?;
    debug!(trigger = %p.trigger_id, node = %p.node_id, "memory.retrieve_context contract");
    let namespace = p.namespace.unwrap_or_default();
    validate_namespace(&namespace)?;
    let q = RetrieveQuery {
        text: p.query,
        top_k: p.top_k,
        session_id: None,
        // #16: 权威租户作用域检索。 #21: namespace 作用域过滤。
        tenant: tenant.to_string(),
        namespace,
        namespace_key: p.namespace_key.unwrap_or_default(),
        tier_filter: None,
        token_budget: default_budget(),
        aggregate: true,
    };
    let mut ctx: FormattedContext = engine
        .retrieve_context(&q)
        .await
        .map_err(|e| RpcError::from_engine(&e))?;
    // M5 #46: 同 retrieve — owner 不可见块剔除后再拼契约结果。
    ctx.blocks.retain(|b| !acl_denies(acl, &b.owner));
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
        queries: Arc<Mutex<Vec<RetrieveQuery>>>,
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
            self.queries.lock().await.push(_q.clone());
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
                    String::new(),
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
        // #33 web surface: canned browse/manage responses (wire-level tests only).
        async fn list_memories(
            &self,
            q: &fm_core::ListQuery,
            _tenant: &str,
        ) -> fm_core::MemoryResult<Vec<MemoryItem>> {
            let item = MemoryItem::new_turn_skeleton(
                "m0".into(),
                "ix0".into(),
                0,
                "s".into(),
                String::new(),
                fm_core::MemoryType::Episodic,
                "browse me".into(),
                1,
            );
            let n = if q.memory_type.as_deref() == Some("Semantic") {
                0
            } else {
                1
            };
            Ok(vec![item; n])
        }
        async fn list_audit_log(
            &self,
            _limit: usize,
        ) -> fm_core::MemoryResult<Vec<fm_core::AuditEntry>> {
            Ok(vec![fm_core::AuditEntry {
                id: 7,
                at: 1,
                actor: "test".into(),
                action: "commit".into(),
                target_id: "m0".into(),
                detail: "{}".into(),
            }])
        }
        async fn list_merges(&self) -> fm_core::MemoryResult<Vec<fm_core::MergeEntry>> {
            Ok(vec![fm_core::MergeEntry {
                id: 3,
                at: 2,
                source_id: "a".into(),
                target_id: "b".into(),
                reason: "dup".into(),
            }])
        }
        async fn unmerge(&self, merge_id: u64) -> fm_core::MemoryResult<bool> {
            Ok(merge_id == 3)
        }
        async fn reconcile(&self) -> fm_core::MemoryResult<usize> {
            Ok(5)
        }
    }

    fn stub_handle() -> (EngineHandle, Arc<Mutex<Vec<String>>>) {
        let (eng, committed, _) = stub_handle_q();
        (eng, committed)
    }

    #[allow(clippy::type_complexity)]
    fn stub_handle_q() -> (
        EngineHandle,
        Arc<Mutex<Vec<String>>>,
        Arc<Mutex<Vec<RetrieveQuery>>>,
    ) {
        let committed = Arc::new(Mutex::new(vec![]));
        let queries = Arc::new(Mutex::new(vec![]));
        (
            EngineHandle::from_concrete(DispatchStub {
                committed: committed.clone(),
                queries: queries.clone(),
            }),
            committed,
            queries,
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
        let resp = dispatch(req.clone(), eng, "", None).await;
        assert!(resp.error.is_none(), "unexpected error: {:?}", resp.error);
        resp.result.expect("missing result")
    }

    async fn dispatch_err(req: &RpcRequest, eng: &EngineHandle) -> RpcError {
        let resp = dispatch(req.clone(), eng, "", None).await;
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

    // #21: namespace 参数透传到 RetrieveQuery; 非法值 -32602。
    #[tokio::test]
    async fn dispatch_retrieve_namespace_passthrough() {
        let (eng, _, queries) = stub_handle_q();
        dispatch_ok(
            &rpc(
                "retrieve",
                serde_json::json!({"text":"hi","namespace":"project","namespace_key":"/p"}),
                30,
            ),
            &eng,
        )
        .await;
        let qs = queries.lock().await;
        assert_eq!(qs.last().unwrap().namespace, "project");
        assert_eq!(qs.last().unwrap().namespace_key, "/p");
    }

    #[tokio::test]
    async fn dispatch_invalid_namespace_rejected() {
        let (eng, _) = stub_handle();
        let ix = serde_json::json!({
            "id":"ix1","session_id":"s","turns":[],"timestamp":1,"metadata":{}
        });
        for (method, params) in [
            (
                "retrieve",
                serde_json::json!({"text":"hi","namespace":"bogus"}),
            ),
            (
                "commit",
                serde_json::json!({"session_id":"s","interaction":ix,"namespace":"bogus"}),
            ),
            ("consolidate", serde_json::json!({"namespace":"bogus"})),
            (
                "memory.retrieve_context",
                serde_json::json!({"query":"q","namespace":"bogus"}),
            ),
        ] {
            let e = dispatch_err(&rpc(method, params, 31), &eng).await;
            assert_eq!(e.code, -32602, "method={method} msg={}", e.message);
        }
    }

    // #21: commit 显式 namespace 参数覆盖 interaction 内值 (服务端权威参数模式)。
    #[tokio::test]
    async fn dispatch_commit_namespace_override() {
        let (eng, committed) = stub_handle();
        let ix = serde_json::json!({
            "id":"ix-ns","session_id":"s","namespace":"user","namespace_key":"u1",
            "turns":[],"timestamp":1,"metadata":{}
        });
        dispatch_ok(
            &rpc(
                "commit",
                serde_json::json!({"session_id":"s","interaction":ix,"namespace":"project","namespace_key":"p1"}),
                32,
            ),
            &eng,
        )
        .await;
        assert!(committed.lock().await.contains(&"ix-ns".to_string()));
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

    // ---- #33 web surface: browse/manage dispatch tests ----

    #[tokio::test]
    async fn dispatch_list_browse() {
        let (eng, _) = stub_handle();
        let v = dispatch_ok(
            &rpc(
                "list",
                serde_json::json!({"offset":0,"limit":10,"memory_type":"Episodic"}),
                16,
            ),
            &eng,
        )
        .await;
        assert_eq!(v.as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn dispatch_list_type_filter_empty() {
        let (eng, _) = stub_handle();
        let v = dispatch_ok(
            &rpc("list", serde_json::json!({"memory_type":"Semantic"}), 17),
            &eng,
        )
        .await;
        assert_eq!(v.as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn dispatch_list_limit_capped() {
        let (eng, _) = stub_handle();
        let e = dispatch_err(&rpc("list", serde_json::json!({"limit":1000}), 18), &eng).await;
        assert_eq!(e.code, -32602);
    }

    #[tokio::test]
    async fn dispatch_list_bad_type_rejected() {
        let (eng, _) = stub_handle();
        let e = dispatch_err(
            &rpc("list", serde_json::json!({"memory_type":"Bogus"}), 19),
            &eng,
        )
        .await;
        assert_eq!(e.code, -32602);
    }

    #[tokio::test]
    async fn dispatch_list_bad_namespace_rejected() {
        let (eng, _) = stub_handle();
        let e = dispatch_err(&rpc("list", serde_json::json!({"namespace":"x"}), 20), &eng).await;
        assert_eq!(e.code, -32602);
    }

    #[tokio::test]
    async fn dispatch_audit_log() {
        let (eng, _) = stub_handle();
        let v = dispatch_ok(&rpc("audit_log", serde_json::json!({"limit":50}), 21), &eng).await;
        assert_eq!(v[0]["action"], "commit");
        assert_eq!(v[0]["target_id"], "m0");
    }

    #[tokio::test]
    async fn dispatch_merges_list() {
        let (eng, _) = stub_handle();
        let v = dispatch_ok(&rpc("merges", serde_json::json!({}), 22), &eng).await;
        assert_eq!(v[0]["id"], 3);
        assert_eq!(v[0]["reason"], "dup");
    }

    #[tokio::test]
    async fn dispatch_unmerge_with_confirm() {
        let (eng, _) = stub_handle();
        let v = dispatch_ok(
            &rpc("unmerge", serde_json::json!({"id":3,"confirm":true}), 23),
            &eng,
        )
        .await;
        assert_eq!(v["unmerged"], true);
    }

    #[tokio::test]
    async fn dispatch_unmerge_without_confirm_rejected() {
        let (eng, _) = stub_handle();
        let e = dispatch_err(&rpc("unmerge", serde_json::json!({"id":3}), 24), &eng).await;
        assert_eq!(e.code, -32602);
    }

    #[tokio::test]
    async fn dispatch_reconcile_with_confirm() {
        let (eng, _) = stub_handle();
        let v = dispatch_ok(
            &rpc("reconcile", serde_json::json!({"confirm":true}), 25),
            &eng,
        )
        .await;
        assert_eq!(v["reconciled"], 5);
    }

    #[tokio::test]
    async fn dispatch_reconcile_without_confirm_rejected() {
        let (eng, _) = stub_handle();
        let e = dispatch_err(&rpc("reconcile", serde_json::json!({}), 26), &eng).await;
        assert_eq!(e.code, -32602);
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
            "",
            None,
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
            "",
            None,
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
                    owner: None,
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

    // ── v1.10.0 #57/#58: revoke / grants / playbook param validation (stub engine). ──

    #[tokio::test]
    async fn revoke_requires_confirm() {
        let (eng, _) = stub_handle();
        let err = dispatch_err(
            &rpc(
                "revoke",
                serde_json::json!({"filter": {"authority": "observed"}}),
                30,
            ),
            &eng,
        )
        .await;
        assert_eq!(err.code, -32602, "B5 confirm required");
    }

    #[tokio::test]
    async fn revoke_rejects_empty_filter() {
        let (eng, _) = stub_handle();
        let err = dispatch_err(
            &rpc(
                "revoke",
                serde_json::json!({"filter": {}, "confirm": true}),
                31,
            ),
            &eng,
        )
        .await;
        // 空过滤 persist 层 fail-closed (Revoke) → Unsupported。
        assert_ne!(err.code, 0, "empty filter must fail closed, got {err:?}");
    }

    #[tokio::test]
    async fn grant_access_validates_keys_and_namespace() {
        let (eng, _) = stub_handle();
        let err = dispatch_err(
            &rpc(
                "grant_access",
                serde_json::json!({"grantor_key": "  ", "grantee_key": "b"}),
                32,
            ),
            &eng,
        )
        .await;
        assert_eq!(err.code, -32602);
        let err = dispatch_err(
            &rpc(
                "grant_access",
                serde_json::json!({"grantor_key": "a", "grantee_key": "b", "namespace": "user"}),
                33,
            ),
            &eng,
        )
        .await;
        assert_eq!(err.code, -32602, "only agent tier grantable");
    }

    #[tokio::test]
    async fn adopt_playbook_validates_namespace() {
        let (eng, _) = stub_handle();
        let err = dispatch_err(
            &rpc(
                "adopt_playbook",
                serde_json::json!({"playbook_id": "p1", "consumer_namespace": "bogus", "consumer_key": "k"}),
                34,
            ),
            &eng,
        )
        .await;
        assert_eq!(err.code, -32602);
    }

    #[tokio::test]
    async fn publish_playbook_rejects_blank_publisher() {
        let (eng, _) = stub_handle();
        let err = dispatch_err(
            &rpc(
                "publish_playbook",
                serde_json::json!({"memory_id": "m1", "publisher_key": " "}),
                35,
            ),
            &eng,
        )
        .await;
        assert_eq!(err.code, -32602);
    }
}
