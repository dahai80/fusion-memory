//! HTTP 服务（axum）。PRD §11.2。
//!
//! 路由（与 UDS 同语义）：
//! POST /v1/memory/commit | retrieve | consolidate | audit | delete
//! GET  /v1/memory/{id}
//! GET  /healthz (公开)
//! 所有 /v1/* 强制 Bearer（B5）。delete 需 body.confirm=true。

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Json;
use serde_json::Value;
use tower_http::trace::TraceLayer;
use tracing::info;

use crate::auth::check_bearer;
use crate::engine_handle::EngineHandle;
use crate::jsonrpc::{dispatch, RpcRequest, RpcResponse};

/// HTTP 服务共享状态。
#[derive(Clone)]
pub struct HttpState {
    pub engine: EngineHandle,
    pub api_key: Arc<String>,
}

/// 建 axum 路由。
pub fn app(state: HttpState) -> axum::Router {
    axum::Router::new()
        .route("/healthz", get(healthz))
        .route("/v1/memory/commit", post(commit))
        .route("/v1/memory/retrieve", post(retrieve))
        .route("/v1/memory/consolidate", post(consolidate))
        .route("/v1/memory/audit", post(audit))
        .route("/v1/memory/delete", post(delete))
        .route("/v1/memory/:id", get(get_memory))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

async fn healthz() -> impl IntoResponse {
    (StatusCode::OK, "ok")
}

/// 鉴权 + 解析 RPC 请求 → dispatch。
async fn handle_rpc(
    State(st): State<HttpState>,
    headers: HeaderMap,
    Json(req): Json<Value>,
) -> Response {
    if let Err(resp) = check_bearer(&headers, &st.api_key) {
        return resp;
    }
    let rpc: RpcRequest = match serde_json::from_value(req) {
        Ok(r) => r,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error":"invalid json-rpc request"})),
            )
                .into_response();
        }
    };
    let resp: RpcResponse = dispatch(&rpc, &st.engine).await;
    Json(resp).into_response()
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

async fn get_memory(
    State(st): State<HttpState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if let Err(resp) = check_bearer(&headers, &st.api_key) {
        return resp;
    }
    let rpc = RpcRequest {
        jsonrpc: "2.0".into(),
        method: "get".into(),
        params: serde_json::json!({"id": id}),
        id: Value::from(0i64),
    };
    let resp = dispatch(&rpc, &st.engine).await;
    Json(resp).into_response()
}

/// 启动 HTTP 监听。cfg.http_ok() 已由调用方保证。
pub async fn serve(state: HttpState, port: u16) -> Result<(), String> {
    let app = app(state);
    let addr = format!("127.0.0.1:{port}");
    info!(%addr, "http server listening");
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .map_err(|e| format!("bind {addr}: {e}"))?;
    axum::serve(listener, app)
        .await
        .map_err(|e| format!("serve: {e}"))?;
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

    fn test_state(api_key: &str) -> (HttpState, Arc<Mutex<Vec<String>>>) {
        let committed = Arc::new(Mutex::new(vec![]));
        let eng = StubEngine {
            committed: committed.clone(),
        };
        let st = HttpState {
            engine: EngineHandle::from_concrete(eng),
            api_key: Arc::new(api_key.into()),
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
        assert_eq!(code, StatusCode::OK);
        assert!(body.contains("-32602"), "body={body}");
    }

    #[tokio::test]
    async fn healthz_open() {
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
        assert_eq!(code, StatusCode::OK);
        assert!(body.contains("-32601"));
    }
}
