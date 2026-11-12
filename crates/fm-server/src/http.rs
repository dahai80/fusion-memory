//! HTTP 服务（axum）。PRD §11.2。
//!
//! 路由（与 UDS 同语义）：
//! POST /v1/memory/commit | retrieve | consolidate | audit | delete
//! POST /v1/memory/delete_scope | count          (issue #2)
//! POST /v1/memory/list | audit_log | merges | unmerge | reconcile   (#33 web surface)
//! POST /v1/memory/import (agent-studio history import, HTTP-only)
//! GET  /v1/memory/{id}
//! GET  /v1/memory/version (公开, P2-4 版本协商)
//! GET  /healthz (公开, §1.7 探活子系统)
//! GET  /app | /app/*path (#33 web UI static shell, web/dist embedded at compile time)
//! All /v1/* (except version) require Bearer (B5). delete/delete_scope/unmerge/reconcile need body.confirm=true.
//! P2-4: API 版本控制 — HTTP 路径 /v1/ 钉版; RPC 方法 v1.<method> 前缀; jsonrpc=="2.0" 校验。

use std::sync::Arc;

use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Json;
use serde_json::Value;
use tower_http::trace::TraceLayer;
use tracing::{info, warn};

use crate::auth::check_bearer;
use crate::engine_handle::EngineHandle;
use crate::identity::{enforce_multi_tenant, CallerCtx, IdentityVerifier};
use crate::jsonrpc::AclUser;
use crate::jsonrpc::{dispatch, http_status_for_error, RpcRequest, RpcResponse};
// P2-4: API 版本号 (jsonrpc::API_VERSION 单一来源)。
use crate::jsonrpc::API_VERSION;
use crate::metrics::HttpMetrics;

/// P0-3: HTTP 请求体上限。8MB 与 UDS MAX_LINE_BYTES 对齐, 防 POST 大 body 内存放大 DoS。
/// 超限 axum 自动返 413 Payload Too Large, 不到 handler。
const MAX_HTTP_BODY_BYTES: usize = 8 * 1024 * 1024;

/// HTTP 服务共享状态。
#[derive(Clone)]
pub struct HttpState {
    pub engine: EngineHandle,
    pub api_key: Arc<String>,
    /// P0-2: HTTP 请求计数/延迟/错误率指标。
    pub metrics: Arc<HttpMetrics>,
    /// #16: 强制 gateway 源 (X-Fusion-Route: gateway-decision), 缺 → 403。
    pub gateway_origin_required: bool,
    /// #16: 无 X-Fusion-Tenant 时的回退租户 (空 = 默认租户)。
    pub default_tenant: Arc<String>,
    /// #18: 多租户模式 — 验 caller JWT (fusion-identity) + 强制 tid==tenant + 拒空 tenant。
    pub multi_tenant: bool,
    /// #18: fusion-identity 验证器 (真实 HTTP + 测试 mock)。多租户模式必配。
    pub verifier: Arc<dyn IdentityVerifier>,
    /// M5 #46: per-memory ACL 开关 (multi_tenant=true 时生效; owner 位来自 JWT sub)。
    pub acl: bool,
    /// #33: server concrete engine (import route reuses it directly — shared SQLite WAL + store
    /// handle avoids a second open hitting the sled lock; embedder matches the server's). None only in route-wiring unit tests (stub dyn engine).
    pub concrete: Option<Arc<fm_engine::MemoryEngine>>,
}

/// 建 axum 路由。
pub fn app(state: HttpState) -> axum::Router {
    axum::Router::new()
        .route("/healthz", get(healthz))
        .route("/metrics", get(metrics_handler))
        .route("/v1/memory/commit", post(commit))
        .route("/v1/memory/retrieve", post(retrieve))
        .route("/v1/memory/consolidate", post(consolidate))
        .route("/v1/memory/audit", post(audit))
        .route("/v1/memory/delete", post(delete))
        .route("/v1/memory/delete_scope", post(delete_scope))
        .route("/v1/memory/count", post(count))
        // #33 web surface: browse/manage routes (shared by the web UI /app and scripts, same semantics as UDS).
        .route("/v1/memory/list", post(list_route))
        .route("/v1/memory/audit_log", post(audit_log_route))
        .route("/v1/memory/merges", post(merges_route))
        .route("/v1/memory/unmerge", post(unmerge_route))
        .route("/v1/memory/reconcile", post(reconcile_route))
        .route("/v1/memory/provenance", post(provenance_route))
        .route("/v1/memory/revisions", post(revisions_route))
        .route("/v1/memory/export", post(export_route))
        .route("/v1/memory/export_skills", post(export_skills_route))
        .route("/v1/memory/config", post(config_route))
        .route("/v1/memory/import", post(import_route))
        // #33: web UI static assets (web/dist embedded at compile time, no runtime file dependency). Public shell; data goes through Bearer.
        .route("/app", get(web_index))
        .route("/app/*path", get(web_asset))
        // P2-4: 版本协商端点 (HTTP 对称 UDS version 方法)。无需 body, GET 直返 api_version。
        .route("/v1/memory/version", get(version))
        .route("/v1/memory/:id", get(get_memory))
        // P0-3: body 上限层, 全路由生效, 超 MAX_HTTP_BODY_BYTES 返 413。
        .layer(DefaultBodyLimit::max(MAX_HTTP_BODY_BYTES))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

/// §1.7: /healthz 不再永远 200。探活 persist+store (经 engine.count, 触 SQLite+store 路径),
/// 失败返 503 + 诊断体。编排器/liveness 据此判定功能性宕机, 而非仅 TCP 存活。
/// count(None) 全量计数 — 轻量 SELECT COUNT(*), 持续暴露 SQLite 锁死/WAL 损坏/store 中毒。
async fn healthz(State(st): State<HttpState>) -> Response {
    match st.engine.count(None).await {
        Ok(n) => {
            info!(count = n, "healthz ok");
            (
                StatusCode::OK,
                Json(serde_json::json!({"status":"ok","count":n})),
            )
                .into_response()
        }
        Err(e) => {
            warn!(error = %e, "healthz probe failed");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({"status":"unhealthy","error":e.to_string()})),
            )
                .into_response()
        }
    }
}

/// P2-4: 版本协商端点。GET /v1/memory/version → {api_version}。公开 (无 Bearer,
/// 客户端鉴权前需知服务端 API 版本)。UDS 侧 `version` 方法对称。
async fn version() -> Response {
    (
        StatusCode::OK,
        Json(serde_json::json!({"api_version": API_VERSION})),
    )
        .into_response()
}

/// P0-2: Prometheus 文本格式 metrics 端点。公开 (不加 Bearer, 同 healthz 供 LB/monitor 探活)。
/// 返: http_requests_total / http_errors_total / http_request_duration_seconds (histogram buckets)
///     + engine 层: engine_embedder_in_flight / engine_consolidate_running / store_pool_in_use。
async fn metrics_handler(State(st): State<HttpState>) -> Response {
    let body = st.metrics.render_prometheus();
    (
        StatusCode::OK,
        [("content-type", "text/plain; version=0.0.4")],
        body,
    )
        .into_response()
}

/// §3.8: axum `Json<Value>` 已解析体, 直接 `from_value` 进 RpcRequest (一次反序列化), 不再二解。
/// §2.9: dispatch 返 RpcResponse; 若 error 已设 → 按 http_status_for_error 映射 HTTP 状态码,
/// 不再无条件 200 把引擎错误埋进 body。
/// P0-2: handle_rpc 经 metrics 计数 (total/error) + 延迟直方图。
async fn handle_rpc(
    State(st): State<HttpState>,
    headers: HeaderMap,
    Json(req): Json<Value>,
) -> Response {
    // #18: multi_tenant 模式下 Bearer 是 JWT, 由 identity verify 校验 (非静态 api_key)。
    // 非 multi_tenant 保留静态 api_key Bearer 校验 (B5 向后兼容)。
    if !st.multi_tenant {
        if let Err(resp) = check_bearer(&headers, &st.api_key) {
            st.metrics.incr_total();
            st.metrics.incr_error();
            return resp;
        }
    }
    // #16: gateway 源校验 + 权威租户提取 (X-Fusion-Tenant > default_tenant > "")。
    let tenant = match crate::tenant::check_gateway_origin(
        &headers,
        st.gateway_origin_required,
        &st.default_tenant,
        false,
        "/v1/memory",
    ) {
        Ok(t) => t,
        Err((code, body)) => {
            st.metrics.incr_total();
            st.metrics.incr_error();
            return (code, body).into_response();
        }
    };
    // #18: 多租户模式 — 验 caller JWT (fusion-identity) + 强制 tid==tenant + 拒空 tenant。
    let ctx: CallerCtx = match enforce_multi_tenant(
        st.verifier.as_ref(),
        &headers,
        &tenant,
        st.multi_tenant,
    )
    .await
    {
        Ok(c) => c,
        Err((code, body)) => {
            st.metrics.incr_total();
            st.metrics.incr_error();
            return (code, body).into_response();
        }
    };
    // M5 #46: ACL 身份仅在 multi_tenant+acl 同时开启时生效 (JWT sub 为 owner 比对值)。
    let acl_user = if st.acl && st.multi_tenant {
        Some(AclUser {
            sub: ctx.sub,
            admin: ctx.role == "admin",
        })
    } else {
        None
    };
    let rpc: RpcRequest = match serde_json::from_value(req) {
        Ok(r) => r,
        Err(e) => {
            st.metrics.incr_total();
            st.metrics.incr_error();
            return (
                StatusCode::BAD_REQUEST,
                Json(
                    serde_json::json!({"error":"invalid json-rpc request","detail":e.to_string()}),
                ),
            )
                .into_response();
        }
    };
    let method = rpc.method.clone();
    let start = std::time::Instant::now();
    let resp: RpcResponse = dispatch(rpc, &st.engine, &tenant, acl_user.as_ref()).await;
    let elapsed = start.elapsed().as_secs_f64();
    st.metrics.incr_total();
    st.metrics.observe_duration(&method, elapsed);
    if resp.error.is_some() {
        st.metrics.incr_error();
    }
    let status = match &resp.error {
        Some(e) => http_status_for_error(e.code),
        None => StatusCode::OK,
    };
    (status, Json(resp)).into_response()
}

async fn commit(st: State<HttpState>, h: HeaderMap, j: Json<Value>) -> Response {
    handle_rpc(st, h, j).await
}
async fn retrieve(st: State<HttpState>, h: HeaderMap, j: Json<Value>) -> Response {
    handle_rpc(st, h, j).await
}
async fn consolidate(st: State<HttpState>, h: HeaderMap, j: Json<Value>) -> Response {
    handle_rpc(st, h, j).await
}
async fn audit(st: State<HttpState>, h: HeaderMap, j: Json<Value>) -> Response {
    handle_rpc(st, h, j).await
}
async fn delete(st: State<HttpState>, h: HeaderMap, j: Json<Value>) -> Response {
    handle_rpc(st, h, j).await
}
async fn delete_scope(st: State<HttpState>, h: HeaderMap, j: Json<Value>) -> Response {
    handle_rpc(st, h, j).await
}
async fn count(st: State<HttpState>, h: HeaderMap, j: Json<Value>) -> Response {
    handle_rpc(st, h, j).await
}

// #33 web surface: browse/manage thin routes (dispatch semantics identical to UDS; all go through handle_rpc).
async fn list_route(st: State<HttpState>, h: HeaderMap, j: Json<Value>) -> Response {
    handle_rpc(st, h, j).await
}
async fn audit_log_route(st: State<HttpState>, h: HeaderMap, j: Json<Value>) -> Response {
    handle_rpc(st, h, j).await
}
async fn merges_route(st: State<HttpState>, h: HeaderMap, j: Json<Value>) -> Response {
    handle_rpc(st, h, j).await
}
async fn unmerge_route(st: State<HttpState>, h: HeaderMap, j: Json<Value>) -> Response {
    handle_rpc(st, h, j).await
}
async fn reconcile_route(st: State<HttpState>, h: HeaderMap, j: Json<Value>) -> Response {
    handle_rpc(st, h, j).await
}
async fn provenance_route(st: State<HttpState>, h: HeaderMap, j: Json<Value>) -> Response {
    handle_rpc(st, h, j).await
}

// M8/M7: revisions + export thin routes (dispatch semantics identical to UDS).
async fn revisions_route(st: State<HttpState>, h: HeaderMap, j: Json<Value>) -> Response {
    handle_rpc(st, h, j).await
}
async fn export_route(st: State<HttpState>, h: HeaderMap, j: Json<Value>) -> Response {
    handle_rpc(st, h, j).await
}
async fn export_skills_route(st: State<HttpState>, h: HeaderMap, j: Json<Value>) -> Response {
    handle_rpc(st, h, j).await
}
// v1.9.0 #53: governance parameter snapshot (read-only).
async fn config_route(st: State<HttpState>, h: HeaderMap, j: Json<Value>) -> Response {
    handle_rpc(st, h, j).await
}

/// #33: agent-studio history import (HTTP version of `fm import`). Builds the import engine
/// on the server's data_dir (same SQLite WAL + store dir as the service), runs synchronously and returns the report. Stub defaults to the server's embedder.
/// Real embedding requires fusion-mlx to be reachable — long runs still return synchronously
/// (report carries imported/skipped/failed counts); chunked progress streaming deferred (recorded as PRD deviation).
async fn import_route(st: State<HttpState>, h: HeaderMap, j: Json<Value>) -> Response {
    if !st.multi_tenant {
        if let Err(resp) = check_bearer(&h, &st.api_key) {
            st.metrics.incr_total();
            st.metrics.incr_error();
            return resp;
        }
    }
    let _tenant = match crate::tenant::check_gateway_origin(
        &h,
        st.gateway_origin_required,
        &st.default_tenant,
        false,
        "/v1/memory/import",
    ) {
        Ok(t) => t,
        Err((code, body)) => {
            st.metrics.incr_total();
            st.metrics.incr_error();
            return (code, body).into_response();
        }
    };
    let p: Value = j.0;
    let source = p
        .get("source")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let source_db = match source {
        Some(s) => s,
        None => match std::env::var("HOME") {
            Ok(hm) => format!("{hm}/.fusion-agent-studio/memory.db"),
            Err(_) => {
                st.metrics.incr_error();
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({"error": "source required when HOME unset"})),
                )
                    .into_response();
            }
        },
    };
    if !std::path::Path::new(&source_db).exists() {
        st.metrics.incr_error();
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": format!("source db not found: {source_db}")})),
        )
            .into_response();
    }
    // #33: reuse the server concrete engine directly — shared SQLite WAL + store handle (a second open
    // hits the sled exclusive lock, verified in smoke), embedder matches the server (stub server = stub import).
    let Some(engine) = st.concrete.as_ref() else {
        st.metrics.incr_error();
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "import unavailable: engine is not the concrete MemoryEngine"})),
        )
            .into_response();
    };
    match fm_engine::import_studio::run_import(engine, &source_db).await {
        Ok(report) => {
            info!(
                imported = report.imported,
                failed = report.failed,
                "http import done"
            );
            (
                StatusCode::OK,
                Json(serde_json::to_value(report).unwrap_or_default()),
            )
                .into_response()
        }
        Err(e) => {
            st.metrics.incr_error();
            warn!(error = %e, "http import failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": e})),
            )
                .into_response()
        }
    }
}

// ── #33: web UI static assets (crates/fm-server/web/dist embedded at compile time). Public shell (index/assets),
// all data requests still go through Bearer-protected /v1/* — without a key the UI only renders the login state.
static WEB_DIST: include_dir::Dir = include_dir::include_dir!("$CARGO_MANIFEST_DIR/web/dist");

fn mime_for(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" => "application/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    }
}

fn serve_embedded(file: &include_dir::File<'static>) -> Response {
    let name = file.path().to_string_lossy().to_string();
    (
        [(axum::http::header::CONTENT_TYPE, mime_for(&name))],
        file.contents().to_vec(),
    )
        .into_response()
}

async fn web_index() -> Response {
    match WEB_DIST.get_file("index.html") {
        Some(f) => serve_embedded(f),
        None => (
            StatusCode::NOT_FOUND,
            "web UI not built (crates/fm-server/web/dist missing)",
        )
            .into_response(),
    }
}

async fn web_asset(Path(path): Path<String>) -> Response {
    match WEB_DIST.get_file(&path) {
        Some(f) => serve_embedded(f),
        // SPA hash routing needs no path fallback; unknown asset = build drift, fail visibly with 404.
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn get_memory(
    State(st): State<HttpState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    // #18: multi_tenant 模式下 Bearer 是 JWT (identity verify 校验), 跳过静态 api_key 校验。
    if !st.multi_tenant {
        if let Err(resp) = check_bearer(&headers, &st.api_key) {
            st.metrics.incr_total();
            st.metrics.incr_error();
            return resp;
        }
    }
    // #16: GET 路径同样做 gateway 源校验 + 租户提取。
    let tenant = match crate::tenant::check_gateway_origin(
        &headers,
        st.gateway_origin_required,
        &st.default_tenant,
        false,
        "/v1/memory/:id",
    ) {
        Ok(t) => t,
        Err((code, body)) => {
            st.metrics.incr_total();
            st.metrics.incr_error();
            return (code, body).into_response();
        }
    };
    // #18: GET 路径同样做多租户身份强制。
    let ctx: CallerCtx = match enforce_multi_tenant(
        st.verifier.as_ref(),
        &headers,
        &tenant,
        st.multi_tenant,
    )
    .await
    {
        Ok(c) => c,
        Err((code, body)) => {
            st.metrics.incr_total();
            st.metrics.incr_error();
            return (code, body).into_response();
        }
    };
    // M5 #46: GET /v1/memory/:id 同 owner 过滤。
    let acl_user = if st.acl && st.multi_tenant {
        Some(AclUser {
            sub: ctx.sub,
            admin: ctx.role == "admin",
        })
    } else {
        None
    };
    let rpc = RpcRequest {
        jsonrpc: "2.0".into(),
        method: "get".into(),
        params: serde_json::json!({"id": id}),
        id: Value::from(0i64),
    };
    let start = std::time::Instant::now();
    let resp = dispatch(rpc, &st.engine, &tenant, acl_user.as_ref()).await;
    st.metrics
        .observe_duration("get", start.elapsed().as_secs_f64());
    st.metrics.incr_total();
    if resp.error.is_some() {
        st.metrics.incr_error();
    }
    let status = match &resp.error {
        Some(e) => http_status_for_error(e.code),
        None => StatusCode::OK,
    };
    (status, Json(resp)).into_response()
}

/// 启动 HTTP 监听。cfg.http_ok() 已由调用方保证。
/// §1.11: shutdown 信号到 → axum graceful drain, 不打断在飞 consolidate saga。
pub async fn serve(
    state: HttpState,
    port: u16,
    shutdown: tokio::sync::oneshot::Receiver<()>,
) -> Result<(), String> {
    let app = app(state);
    let addr = format!("127.0.0.1:{port}");
    info!(%addr, "http server listening");
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .map_err(|e| format!("bind {addr}: {e}"))?;
    // §1.11: with_graceful_shutdown 让在飞请求 drain 完再退, 不被 SIGTERM 打断 saga。
    let serve_fut = axum::serve(listener, app).with_graceful_shutdown(async move {
        let _ = shutdown.await;
        info!("http graceful shutdown triggered");
    });
    serve_fut.await.map_err(|e| format!("serve: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use fm_core::{
        ConsolidationReport, FormattedContext, Interaction, MemoryId, MemoryItem, RetrieveQuery,
    };
    use std::sync::Arc;
    use tokio::sync::Mutex;

    // 最小 stub 引擎，测路由 + 鉴权（不连 mlx）。
    struct StubEngine {
        committed: Arc<Mutex<Vec<String>>>,
    }
    #[async_trait::async_trait]
    impl fm_core::FusionMemoryEngine for StubEngine {
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
        async fn delete_scope(&self, _scope: &str) -> fm_core::MemoryResult<u64> {
            Ok(2)
        }
        async fn count(&self, _scope: Option<&str>) -> fm_core::MemoryResult<u64> {
            Ok(7)
        }
        async fn audit_memory_access(
            &self,
            _e: &[String],
        ) -> fm_core::MemoryResult<Vec<MemoryItem>> {
            Ok(vec![])
        }
        // #33 web surface: canned browse/manage (route wiring tests).
        async fn list_memories(
            &self,
            _q: &fm_core::ListQuery,
            _tenant: &str,
        ) -> fm_core::MemoryResult<Vec<MemoryItem>> {
            Ok(vec![MemoryItem::new_turn_skeleton(
                "m0".into(),
                "ix0".into(),
                0,
                "s".into(),
                String::new(),
                fm_core::MemoryType::Episodic,
                "browse me".into(),
                1,
            )])
        }
        async fn list_audit_log(
            &self,
            _limit: usize,
        ) -> fm_core::MemoryResult<Vec<fm_core::AuditEntry>> {
            Ok(vec![fm_core::AuditEntry {
                id: 1,
                at: 1,
                actor: "test".into(),
                action: "commit".into(),
                target_id: "m0".into(),
                detail: "{}".into(),
            }])
        }
        async fn list_merges(&self) -> fm_core::MemoryResult<Vec<fm_core::MergeEntry>> {
            Ok(vec![])
        }
        async fn unmerge(&self, _merge_id: u64) -> fm_core::MemoryResult<bool> {
            Ok(true)
        }
        async fn reconcile(&self) -> fm_core::MemoryResult<usize> {
            Ok(3)
        }
    }

    fn test_state(api_key: &str) -> (HttpState, Arc<Mutex<Vec<String>>>) {
        let committed = Arc::new(Mutex::new(vec![]));
        let eng = StubEngine {
            committed: committed.clone(),
        };
        let st = HttpState {
            engine: EngineHandle::from_concrete(eng),
            api_key: Arc::new(api_key.into()),
            metrics: crate::metrics::HttpMetrics::new(),
            gateway_origin_required: false,
            default_tenant: Arc::new(String::new()),
            multi_tenant: false,
            acl: false,
            verifier: crate::identity::noop_verifier(),
            concrete: None,
        };
        (st, committed)
    }

    async fn req(
        app: &axum::Router,
        path: &str,
        body: &str,
        auth: Option<&str>,
    ) -> (StatusCode, String) {
        let mut b = Request::builder().method("POST").uri(path);
        if let Some(a) = auth {
            b = b.header("authorization", a);
        }
        let r = b
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        let resp = tower::ServiceExt::oneshot(app.clone(), r).await.unwrap();
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
            .await
            .unwrap();
        (status, String::from_utf8_lossy(&bytes).into_owned())
    }

    #[tokio::test]
    async fn commit_with_valid_token() {
        let (st, committed) = test_state("sekret");
        let app = app(st);
        let body = r#"{"jsonrpc":"2.0","method":"commit","params":{"session_id":"s","interaction":{"id":"ix1","session_id":"s","turns":[{"turn_idx":0,"user_message":"hi","assistant_message":"yo","tool_calls":[]}],"timestamp":1,"metadata":{}}},"id":1}"#;
        let (code, body) = req(&app, "/v1/memory/commit", body, Some("Bearer sekret")).await;
        assert_eq!(code, StatusCode::OK, "{body}");
        assert!(committed.lock().await.contains(&"ix1".to_string()));
    }

    #[tokio::test]
    async fn commit_without_token_rejected() {
        let (st, _) = test_state("sekret");
        let app = app(st);
        let body = r#"{"jsonrpc":"2.0","method":"commit","params":{},"id":1}"#;
        let (code, _) = req(&app, "/v1/memory/commit", body, None).await;
        assert_eq!(code, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn commit_wrong_token_rejected() {
        let (st, _) = test_state("sekret");
        let app = app(st);
        let body = r#"{"jsonrpc":"2.0","method":"commit","params":{},"id":1}"#;
        let (code, _) = req(&app, "/v1/memory/commit", body, Some("Bearer nope")).await;
        assert_eq!(code, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn delete_without_confirm_rejected() {
        let (st, _) = test_state("sekret");
        let app = app(st);
        let body = r#"{"jsonrpc":"2.0","method":"delete","params":{"id":"m1"},"id":1}"#;
        let (code, body) = req(&app, "/v1/memory/delete", body, Some("Bearer sekret")).await;
        // §2.9: invalid_params(-32602) → 400, 不再 200 埋进 body
        assert_eq!(code, StatusCode::BAD_REQUEST);
        assert!(body.contains("-32602"), "body={body}");
    }

    #[tokio::test]
    async fn get_memory_route() {
        let (st, _) = test_state("sekret");
        let app = app(st);
        let r = Request::builder()
            .method("GET")
            .uri("/v1/memory/m0")
            .header("authorization", "Bearer sekret")
            .body(Body::empty())
            .unwrap();
        let resp = tower::ServiceExt::oneshot(app, r).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    // ---- #33 web surface: browse/manage routes ----

    #[tokio::test]
    async fn list_route_ok() {
        let (st, _) = test_state("sekret");
        let (code, body) = req(
            &app(st),
            "/v1/memory/list",
            r#"{"jsonrpc":"2.0","method":"list","params":{"offset":0,"limit":10},"id":1}"#,
            Some("Bearer sekret"),
        )
        .await;
        assert_eq!(code, StatusCode::OK, "body: {body}");
        assert!(body.contains("browse me"), "body: {body}");
    }

    #[tokio::test]
    async fn list_route_without_token_rejected() {
        let (st, _) = test_state("sekret");
        let (code, _) = req(&app(st), "/v1/memory/list", "{}", None).await;
        assert_eq!(code, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn merges_and_reconcile_routes_ok() {
        let (st, _) = test_state("sekret");
        let (code, _) = req(
            &app(st.clone()),
            "/v1/memory/merges",
            r#"{"jsonrpc":"2.0","method":"merges","params":{},"id":1}"#,
            Some("Bearer sekret"),
        )
        .await;
        assert_eq!(code, StatusCode::OK);
        let (code, body) = req(
            &app(st.clone()),
            "/v1/memory/reconcile",
            r#"{"jsonrpc":"2.0","method":"reconcile","params":{"confirm":true},"id":1}"#,
            Some("Bearer sekret"),
        )
        .await;
        assert_eq!(code, StatusCode::OK, "body: {body}");
        assert!(body.contains("reconciled"), "body: {body}");
        let (code, _) = req(
            &app(st),
            "/v1/memory/reconcile",
            r#"{"jsonrpc":"2.0","method":"reconcile","params":{},"id":1}"#,
            Some("Bearer sekret"),
        )
        .await;
        assert_eq!(code, StatusCode::BAD_REQUEST, "confirm required (B5)");
    }

    #[tokio::test]
    async fn app_shell_served_and_unknown_asset_404() {
        let (st, _) = test_state("sekret");
        let a = app(st);
        let r = Request::builder()
            .method("GET")
            .uri("/app")
            .body(Body::empty())
            .unwrap();
        let resp = tower::ServiceExt::oneshot(a.clone(), r).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
            .await
            .unwrap();
        let html = String::from_utf8_lossy(&bytes).into_owned();
        assert!(html.contains("fusion-memory console"), "index served");
        // static shell is public (no Bearer); data plane /v1/* still requires Bearer (proven by list_without_token above).
        let r = Request::builder()
            .method("GET")
            .uri("/app/assets/no-such-asset.js")
            .body(Body::empty())
            .unwrap();
        let resp = tower::ServiceExt::oneshot(a, r).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn import_route_missing_source_404() {
        let (st, _) = test_state("sekret");
        let (code, body) = req(
            &app(st),
            "/v1/memory/import",
            r#"{"source":"/tmp/fm-no-such-src-db/memory.db"}"#,
            Some("Bearer sekret"),
        )
        .await;
        assert_eq!(code, StatusCode::NOT_FOUND, "body: {body}");
        assert!(body.contains("not found"), "body: {body}");
    }

    #[tokio::test]
    async fn get_memory_without_token_rejected() {
        let (st, _) = test_state("sekret");
        let app = app(st);
        let r = Request::builder()
            .method("GET")
            .uri("/v1/memory/m0")
            .body(Body::empty())
            .unwrap();
        let resp = tower::ServiceExt::oneshot(app, r).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn retrieve_route() {
        let (st, _) = test_state("sekret");
        let app = app(st);
        let body = r#"{"jsonrpc":"2.0","method":"retrieve","params":{"text":"hi"},"id":1}"#;
        let (code, _) = req(&app, "/v1/memory/retrieve", body, Some("Bearer sekret")).await;
        assert_eq!(code, StatusCode::OK);
    }

    #[tokio::test]
    async fn consolidate_route() {
        let (st, _) = test_state("sekret");
        let app = app(st);
        let body = r#"{"jsonrpc":"2.0","method":"consolidate","params":{},"id":1}"#;
        let (code, _) = req(&app, "/v1/memory/consolidate", body, Some("Bearer sekret")).await;
        assert_eq!(code, StatusCode::OK);
    }

    #[tokio::test]
    async fn delete_scope_route_with_confirm() {
        let (st, _) = test_state("sekret");
        let app = app(st);
        let body = r#"{"jsonrpc":"2.0","method":"delete_scope","params":{"scope":"sess-A","confirm":true},"id":1}"#;
        let (code, body) = req(&app, "/v1/memory/delete_scope", body, Some("Bearer sekret")).await;
        assert_eq!(code, StatusCode::OK, "{body}");
        assert!(body.contains("deleted_count"), "body={body}");
    }

    #[tokio::test]
    async fn delete_scope_route_without_confirm_rejected() {
        let (st, _) = test_state("sekret");
        let app = app(st);
        let body =
            r#"{"jsonrpc":"2.0","method":"delete_scope","params":{"scope":"sess-A"},"id":1}"#;
        let (code, body) = req(&app, "/v1/memory/delete_scope", body, Some("Bearer sekret")).await;
        // §2.9: invalid_params(-32602) → 400
        assert_eq!(code, StatusCode::BAD_REQUEST);
        assert!(body.contains("-32602"), "body={body}");
    }

    #[tokio::test]
    async fn count_route() {
        let (st, _) = test_state("sekret");
        let app = app(st);
        let body = r#"{"jsonrpc":"2.0","method":"count","params":{},"id":1}"#;
        let (code, body) = req(&app, "/v1/memory/count", body, Some("Bearer sekret")).await;
        assert_eq!(code, StatusCode::OK, "{body}");
        assert!(body.contains("count"), "body={body}");
    }

    #[tokio::test]
    async fn count_route_without_token_rejected() {
        let (st, _) = test_state("sekret");
        let app = app(st);
        let body = r#"{"jsonrpc":"2.0","method":"count","params":{},"id":1}"#;
        let (code, _) = req(&app, "/v1/memory/count", body, None).await;
        assert_eq!(code, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn audit_route() {
        let (st, _) = test_state("sekret");
        let app = app(st);
        let body = r#"{"jsonrpc":"2.0","method":"audit","params":{"entity_ids":["e1"]},"id":1}"#;
        let (code, _) = req(&app, "/v1/memory/audit", body, Some("Bearer sekret")).await;
        assert_eq!(code, StatusCode::OK);
    }

    #[tokio::test]
    async fn malformed_json_rejected() {
        let (st, _) = test_state("sekret");
        let app = app(st);
        let (code, _body) = req(&app, "/v1/memory/commit", "not-json", Some("Bearer sekret")).await;
        // 非 JSON 体被 axum Json 提取器拒（400），不到 handler 的 invalid json-rpc 分支
        assert_eq!(code, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn valid_json_bad_rpc_structure_rejected() {
        // 合法 JSON 但缺 method 字段 → serde 反序列化 RpcRequest 失败 → handler 400
        let (st, _) = test_state("sekret");
        let app = app(st);
        let (code, body) = req(
            &app,
            "/v1/memory/commit",
            r#"{"foo":"bar"}"#,
            Some("Bearer sekret"),
        )
        .await;
        assert_eq!(code, StatusCode::BAD_REQUEST, "{body}");
        assert!(body.contains("invalid json-rpc"));
    }

    #[tokio::test]
    async fn delete_with_confirm_ok() {
        let (st, _) = test_state("sekret");
        let app = app(st);
        let body =
            r#"{"jsonrpc":"2.0","method":"delete","params":{"id":"m0","confirm":true},"id":1}"#;
        let (code, body) = req(&app, "/v1/memory/delete", body, Some("Bearer sekret")).await;
        assert_eq!(code, StatusCode::OK, "{body}");
        assert!(body.contains("deleted"));
    }

    #[tokio::test]
    async fn method_not_found_returns_error() {
        let (st, _) = test_state("sekret");
        let app = app(st);
        let body = r#"{"jsonrpc":"2.0","method":"bogus","params":{},"id":1}"#;
        let (code, body) = req(&app, "/v1/memory/commit", body, Some("Bearer sekret")).await;
        // §2.9: method_not_found(-32601) → 400, 不再 200 (缺陷被烤进契约, 现修正)
        assert_eq!(code, StatusCode::BAD_REQUEST);
        assert!(body.contains("-32601"));
    }

    // §1.7: /healthz 探活子系统。stub engine.count 返 Ok(7) → 200; 坏引擎 → 503。
    #[tokio::test]
    async fn healthz_probe_ok() {
        let (st, _) = test_state("sekret");
        let app = app(st);
        let r = Request::builder()
            .method("GET")
            .uri("/healthz")
            .body(Body::empty())
            .unwrap();
        let resp = tower::ServiceExt::oneshot(app, r).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    // P2-4: /v1/memory/version 公开端点, 客户端鉴权前协商 API 版本。
    #[tokio::test]
    async fn p2_4_version_endpoint_public() {
        let (st, _) = test_state("sekret");
        let app = app(st);
        let r = Request::builder()
            .method("GET")
            .uri("/v1/memory/version")
            .body(Body::empty())
            .unwrap();
        let resp = tower::ServiceExt::oneshot(app, r).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["api_version"], API_VERSION);
    }

    // P0-3: body 超 8MB → 413 Payload Too Large, 不到 handler。
    #[tokio::test]
    async fn body_over_limit_returns_413() {
        let (st, _) = test_state("sekret");
        let app = app(st);
        let big = "x".repeat(MAX_HTTP_BODY_BYTES + 1);
        let (code, _) = req(
            &app,
            "/v1/memory/commit",
            &format!(
                r#"{{"jsonrpc":"2.0","method":"commit","params":{{}},"padding":"{big}"}},"id":1}}"#
            ),
            Some("Bearer sekret"),
        )
        .await;
        assert_eq!(code, StatusCode::PAYLOAD_TOO_LARGE);
    }

    // P0-2: /metrics 端点返 Prometheus 文本格式, 含计数器。
    #[tokio::test]
    async fn metrics_endpoint_returns_prometheus_text() {
        let (st, _) = test_state("sekret");
        let app = app(st.clone());
        // 先打一个 commit 让计数器 +1
        let body = r#"{"jsonrpc":"2.0","method":"commit","params":{"session_id":"s","interaction":{"id":"ix1","session_id":"s","turns":[{"turn_idx":0,"user_message":"hi","assistant_message":"yo","tool_calls":[]}],"timestamp":1,"metadata":{}}},"id":1}"#;
        let _ = req(&app, "/v1/memory/commit", body, Some("Bearer sekret")).await;
        // 拉 metrics
        let r = Request::builder()
            .method("GET")
            .uri("/metrics")
            .body(Body::empty())
            .unwrap();
        let resp = tower::ServiceExt::oneshot(app, r).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
            .await
            .unwrap();
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("http_requests_total"), "text={text}");
        assert!(
            text.contains("http_request_duration_seconds_count"),
            "text={text}"
        );
    }

    // P0-2: /metrics 无需 Bearer (供 monitor 探活, 同 healthz)。
    #[tokio::test]
    async fn metrics_endpoint_no_auth_required() {
        let (st, _) = test_state("sekret");
        let app = app(st);
        let r = Request::builder()
            .method("GET")
            .uri("/metrics")
            .body(Body::empty())
            .unwrap();
        let resp = tower::ServiceExt::oneshot(app, r).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    // §1.7: 坏引擎 (count 抛错) → healthz 503, 不再永远 200。
    #[tokio::test]
    async fn healthz_probe_unhealthy_503() {
        struct SickEngine;
        #[async_trait::async_trait]
        impl fm_core::FusionMemoryEngine for SickEngine {
            async fn commit_episodic_memory(
                &self,
                _: &str,
                _: &fm_core::Interaction,
            ) -> fm_core::MemoryResult<Vec<fm_core::MemoryId>> {
                Ok(vec![])
            }
            async fn retrieve_context(
                &self,
                _: &fm_core::RetrieveQuery,
            ) -> fm_core::MemoryResult<fm_core::FormattedContext> {
                Ok(fm_core::FormattedContext {
                    blocks: vec![],
                    total_tokens: 0,
                    stale_read: false,
                    last_sync_at: 0,
                })
            }
            async fn consolidate_memories(
                &self,
            ) -> fm_core::MemoryResult<fm_core::ConsolidationReport> {
                Ok(fm_core::ConsolidationReport::default())
            }
            async fn get_memory(
                &self,
                _: &str,
            ) -> fm_core::MemoryResult<Option<fm_core::MemoryItem>> {
                Ok(None)
            }
            async fn delete_memory(&self, _: &str) -> fm_core::MemoryResult<()> {
                Ok(())
            }
            async fn audit_memory_access(
                &self,
                _: &[String],
            ) -> fm_core::MemoryResult<Vec<fm_core::MemoryItem>> {
                Ok(vec![])
            }
            async fn count(&self, _: Option<&str>) -> fm_core::MemoryResult<u64> {
                Err(fm_core::MemoryError::Sqlite(
                    "persist conn lock poisoned".into(),
                ))
            }
        }
        let st = HttpState {
            engine: EngineHandle::from_concrete(SickEngine),
            api_key: Arc::new("sekret".into()),
            metrics: crate::metrics::HttpMetrics::new(),
            gateway_origin_required: false,
            default_tenant: Arc::new(String::new()),
            multi_tenant: false,
            acl: false,
            verifier: crate::identity::noop_verifier(),
            concrete: None,
        };
        let app = app(st);
        let r = Request::builder()
            .method("GET")
            .uri("/healthz")
            .body(Body::empty())
            .unwrap();
        let resp = tower::ServiceExt::oneshot(app, r).await.unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    // §2.9: 引擎 NotFound → HTTP 404 (旧版恒 200)。
    #[tokio::test]
    async fn not_found_returns_404() {
        struct NotFoundEngine;
        #[async_trait::async_trait]
        impl fm_core::FusionMemoryEngine for NotFoundEngine {
            async fn commit_episodic_memory(
                &self,
                _: &str,
                _: &fm_core::Interaction,
            ) -> fm_core::MemoryResult<Vec<fm_core::MemoryId>> {
                Ok(vec![])
            }
            async fn retrieve_context(
                &self,
                _: &fm_core::RetrieveQuery,
            ) -> fm_core::MemoryResult<fm_core::FormattedContext> {
                Err(fm_core::MemoryError::NotFound("vector missing".into()))
            }
            async fn consolidate_memories(
                &self,
            ) -> fm_core::MemoryResult<fm_core::ConsolidationReport> {
                Ok(fm_core::ConsolidationReport::default())
            }
            async fn get_memory(
                &self,
                _: &str,
            ) -> fm_core::MemoryResult<Option<fm_core::MemoryItem>> {
                Ok(None)
            }
            async fn delete_memory(&self, _: &str) -> fm_core::MemoryResult<()> {
                Ok(())
            }
            async fn audit_memory_access(
                &self,
                _: &[String],
            ) -> fm_core::MemoryResult<Vec<fm_core::MemoryItem>> {
                Ok(vec![])
            }
            async fn count(&self, _: Option<&str>) -> fm_core::MemoryResult<u64> {
                Ok(0)
            }
        }
        let st = HttpState {
            engine: EngineHandle::from_concrete(NotFoundEngine),
            api_key: Arc::new("sekret".into()),
            metrics: crate::metrics::HttpMetrics::new(),
            gateway_origin_required: false,
            default_tenant: Arc::new(String::new()),
            multi_tenant: false,
            acl: false,
            verifier: crate::identity::noop_verifier(),
            concrete: None,
        };
        let app = app(st);
        let body = r#"{"jsonrpc":"2.0","method":"retrieve","params":{"text":"hi"},"id":1}"#;
        let (code, body) = req(&app, "/v1/memory/retrieve", body, Some("Bearer sekret")).await;
        assert_eq!(code, StatusCode::NOT_FOUND);
        assert!(body.contains("-32001"), "body={body}");
    }

    // #16: gateway 源强制 — gateway_origin_required=true 且缺 X-Fusion-Route → 403。
    #[tokio::test]
    async fn gateway_origin_required_rejects_missing_route() {
        let (st, _) = test_state("sekret");
        let st = HttpState {
            gateway_origin_required: true,
            ..st
        };
        let app = app(st);
        let body = r#"{"jsonrpc":"2.0","method":"count","params":{},"id":1}"#;
        let (code, b) = req(&app, "/v1/memory/count", body, Some("Bearer sekret")).await;
        assert_eq!(code, StatusCode::FORBIDDEN, "{b}");
        assert!(b.contains("gateway-origin required"), "body={b}");
    }

    // #16: 带正确 X-Fusion-Route: gateway-decision → 放行 (仍过 Bearer)。
    #[tokio::test]
    async fn gateway_origin_required_accepts_valid_route() {
        let (st, _) = test_state("sekret");
        let st = HttpState {
            gateway_origin_required: true,
            ..st
        };
        let app = app(st);
        let body = r#"{"jsonrpc":"2.0","method":"count","params":{},"id":1}"#;
        let mut b = Request::builder().method("POST").uri("/v1/memory/count");
        b = b.header("authorization", "Bearer sekret");
        b = b.header("X-Fusion-Route", "gateway-decision");
        b = b.header("content-type", "application/json");
        let r = b.body(Body::from(body.to_string())).unwrap();
        let resp = tower::ServiceExt::oneshot(app.clone(), r).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    // #16: gateway_origin_required=false (默认) → 无 X-Fusion-Route 也放行 (向后兼容)。
    #[tokio::test]
    async fn gateway_origin_not_required_passes_without_route() {
        let (st, _) = test_state("sekret");
        // test_state 默认 gateway_origin_required=false
        let app = app(st);
        let body = r#"{"jsonrpc":"2.0","method":"count","params":{},"id":1}"#;
        let (code, _) = req(&app, "/v1/memory/count", body, Some("Bearer sekret")).await;
        assert_eq!(code, StatusCode::OK);
    }

    // #16: 权威租户头 X-Fusion-Tenant 透传 — cross-tenant get → NotFound (stub override)。
    #[tokio::test]
    async fn tenant_header_enforces_cross_tenant_isolation() {
        // tenant-aware stub: item m0 属 tenant "acme", 跨租户 get → None。
        struct TenantStub;
        #[async_trait::async_trait]
        impl fm_core::FusionMemoryEngine for TenantStub {
            async fn commit_episodic_memory(
                &self,
                _: &str,
                _: &fm_core::Interaction,
            ) -> fm_core::MemoryResult<Vec<fm_core::MemoryId>> {
                Ok(vec![])
            }
            async fn retrieve_context(
                &self,
                _: &fm_core::RetrieveQuery,
            ) -> fm_core::MemoryResult<fm_core::FormattedContext> {
                Ok(fm_core::FormattedContext {
                    blocks: vec![],
                    total_tokens: 0,
                    stale_read: false,
                    last_sync_at: 0,
                })
            }
            async fn consolidate_memories(
                &self,
            ) -> fm_core::MemoryResult<fm_core::ConsolidationReport> {
                Ok(fm_core::ConsolidationReport::default())
            }
            async fn get_memory(
                &self,
                _: &str,
            ) -> fm_core::MemoryResult<Option<fm_core::MemoryItem>> {
                Ok(None)
            }
            async fn delete_memory(&self, _: &str) -> fm_core::MemoryResult<()> {
                Ok(())
            }
            async fn delete_scope(&self, _: &str) -> fm_core::MemoryResult<u64> {
                Ok(0)
            }
            async fn count(&self, _: Option<&str>) -> fm_core::MemoryResult<u64> {
                Ok(0)
            }
            async fn audit_memory_access(
                &self,
                _: &[String],
            ) -> fm_core::MemoryResult<Vec<fm_core::MemoryItem>> {
                Ok(vec![])
            }
            // #16 覆写: item m0 属 tenant "acme", 跨租户请求 → None (不可见)。
            async fn get_memory_tenant(
                &self,
                id: &str,
                tenant: &str,
            ) -> fm_core::MemoryResult<Option<fm_core::MemoryItem>> {
                if id != "m0" {
                    return Ok(None);
                }
                let item = fm_core::MemoryItem::new_turn_skeleton(
                    "m0".into(),
                    "ix0".into(),
                    0,
                    "s".into(),
                    "acme".into(),
                    fm_core::MemoryType::Episodic,
                    "secret".into(),
                    1,
                );
                if !tenant.is_empty() && tenant != "acme" {
                    return Ok(None);
                }
                Ok(Some(item))
            }
        }
        let st = HttpState {
            engine: EngineHandle::from_concrete(TenantStub),
            api_key: Arc::new("sekret".into()),
            metrics: crate::metrics::HttpMetrics::new(),
            gateway_origin_required: false,
            default_tenant: Arc::new(String::new()),
            multi_tenant: false,
            acl: false,
            verifier: crate::identity::noop_verifier(),
            concrete: None,
        };
        let app = app(st);

        // 本租户 (X-Fusion-Tenant: acme) → 200 + result 含 m0
        let r = Request::builder()
            .method("GET")
            .uri("/v1/memory/m0")
            .header("authorization", "Bearer sekret")
            .header("X-Fusion-Tenant", "acme")
            .body(Body::empty())
            .unwrap();
        let resp = tower::ServiceExt::oneshot(app.clone(), r).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        // 跨租户 (X-Fusion-Tenant: other) → result null (不可见, 不泄露存在性)
        let r = Request::builder()
            .method("GET")
            .uri("/v1/memory/m0")
            .header("authorization", "Bearer sekret")
            .header("X-Fusion-Tenant", "other")
            .body(Body::empty())
            .unwrap();
        let resp = tower::ServiceExt::oneshot(app, r).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(v["result"].is_null(), "cross-tenant must be null, got {v}");
    }

    // #18: 多租户 HTTP 集成 — mock verifier (不起 fusion-identity)。
    struct FixedTidVerifier {
        tid: String,
        ok: bool,
        usage_calls: Arc<Mutex<Vec<(String, String, u64)>>>,
    }
    #[async_trait::async_trait]
    impl crate::identity::IdentityVerifier for FixedTidVerifier {
        async fn verify(&self, _jwt: &str) -> crate::identity::VerifyResult {
            if self.ok {
                Ok(CallerCtx {
                    tid: self.tid.clone(),
                    sub: String::new(),
                    role: String::new(),
                })
            } else {
                Err((
                    StatusCode::UNAUTHORIZED,
                    Json(serde_json::json!({ "error": "invalid token" })),
                ))
            }
        }
        async fn report_usage(&self, t: &str, m: &str, v: u64) -> () {
            self.usage_calls.lock().await.push((t.into(), m.into(), v));
        }
    }

    fn multi_tenant_state(tid: &str, ok: bool) -> HttpState {
        let committed = Arc::new(Mutex::new(vec![]));
        let eng = StubEngine {
            committed: committed.clone(),
        };
        HttpState {
            engine: EngineHandle::from_concrete(eng),
            api_key: Arc::new("sekret".into()),
            metrics: crate::metrics::HttpMetrics::new(),
            gateway_origin_required: false,
            default_tenant: Arc::new(String::new()),
            multi_tenant: true,
            acl: false,
            verifier: Arc::new(FixedTidVerifier {
                tid: tid.into(),
                ok,
                usage_calls: Arc::new(Mutex::new(vec![])),
            }),
            concrete: None,
        }
    }

    // #18 acceptance: caller asserting tenant B with tenant-A token → 403 denied。
    #[tokio::test]
    async fn multi_tenant_cross_tenant_jwt_denied() {
        // jwt tid=acme, request tenant=other → 403
        let st = multi_tenant_state("acme", true);
        let app = app(st);
        let body = r#"{"jsonrpc":"2.0","method":"count","params":{},"id":1}"#;
        let r = Request::builder()
            .method("POST")
            .uri("/v1/memory/count")
            .header("authorization", "Bearer sekret")
            .header("X-Fusion-Tenant", "other")
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        let resp = tower::ServiceExt::oneshot(app, r).await.unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    // #18 acceptance: empty tenant in multi-tenant mode → 401 (red line 1, no default-tenant degradation)。
    #[tokio::test]
    async fn multi_tenant_empty_tenant_rejected() {
        let st = multi_tenant_state("acme", true);
        let app = app(st);
        let body = r#"{"jsonrpc":"2.0","method":"count","params":{},"id":1}"#;
        // 无 X-Fusion-Tenant → tenant="" → 401
        let r = Request::builder()
            .method("POST")
            .uri("/v1/memory/count")
            .header("authorization", "Bearer sekret")
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        let resp = tower::ServiceExt::oneshot(app, r).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    // #18 acceptance: matching tid + tenant → 200 (pass)。
    #[tokio::test]
    async fn multi_tenant_matching_tid_passes() {
        let st = multi_tenant_state("acme", true);
        let app = app(st);
        let body = r#"{"jsonrpc":"2.0","method":"count","params":{},"id":1}"#;
        let r = Request::builder()
            .method("POST")
            .uri("/v1/memory/count")
            .header("authorization", "Bearer good-jwt")
            .header("X-Fusion-Tenant", "acme")
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        let resp = tower::ServiceExt::oneshot(app, r).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    // #18 acceptance: invalid JWT (identity rejects) → 401。
    #[tokio::test]
    async fn multi_tenant_invalid_jwt_rejected() {
        let st = multi_tenant_state("acme", false);
        let app = app(st);
        let body = r#"{"jsonrpc":"2.0","method":"count","params":{},"id":1}"#;
        let r = Request::builder()
            .method("POST")
            .uri("/v1/memory/count")
            .header("authorization", "Bearer bad-jwt")
            .header("X-Fusion-Tenant", "acme")
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        let resp = tower::ServiceExt::oneshot(app, r).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    // #18 acceptance: missing bearer JWT in multi-tenant mode → 401。
    #[tokio::test]
    async fn multi_tenant_missing_jwt_rejected() {
        let st = multi_tenant_state("acme", true);
        let app = app(st);
        let body = r#"{"jsonrpc":"2.0","method":"count","params":{},"id":1}"#;
        // Bearer 是 api_key (sekret) 不是 JWT — multi_tenant 提取 Bearer 后当 JWT 验,
        // mock verifier 返 tid=acme != ... 实际 tenant 头缺 → 先 401 empty。这里测无 tenant 头。
        let r = Request::builder()
            .method("POST")
            .uri("/v1/memory/count")
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        let resp = tower::ServiceExt::oneshot(app, r).await.unwrap();
        // 无 api_key bearer → check_bearer 先拒 401
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    // #18: GET 路径同样强制多租户 — cross-tenant get → 403。
    #[tokio::test]
    async fn multi_tenant_get_cross_tenant_denied() {
        let st = multi_tenant_state("acme", true);
        let app = app(st);
        let r = Request::builder()
            .method("GET")
            .uri("/v1/memory/m0")
            .header("authorization", "Bearer good-jwt")
            .header("X-Fusion-Tenant", "other")
            .body(Body::empty())
            .unwrap();
        let resp = tower::ServiceExt::oneshot(app, r).await.unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    // #18: multi_tenant=false (默认) → 无 JWT 验, 向后兼容 (既有测试已覆盖, 此处显式断言)。
    #[tokio::test]
    async fn single_tenant_mode_skips_identity_verify() {
        let (st, _) = test_state("sekret");
        // test_state 默认 multi_tenant=false + noop verifier
        let app = app(st);
        let body = r#"{"jsonrpc":"2.0","method":"count","params":{},"id":1}"#;
        let (code, _) = req(&app, "/v1/memory/count", body, Some("Bearer sekret")).await;
        assert_eq!(code, StatusCode::OK);
    }
}
