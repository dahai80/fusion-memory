//! 离线集成测试: 真服务链路 (stub engine 经 HTTP app 往返)。
//! 覆盖 engine_builder stub 分支 + http handle_rpc + jsonrpc dispatch + LocalStore trait
//! 在 fm-server binary 实例化被调 (消除单态化 0 计数假象)。
//! PRD §11.2 验收: commit/retrieve/consolidate HTTP 往返 + 鉴权。

use std::sync::Arc;

use axum::body::Body;
use axum::http::Request;
use fm_server::http::HttpState;
use fm_server::{build_server_engine, EngineHandle};
use tempfile::tempdir;
use tower::ServiceExt;

fn stub_state() -> HttpState {
    let dir = tempdir().expect("tempdir");
    let cfg = fm_server::ServerConfig {
        data_dir: dir.path().to_path_buf(),
        http_port: 0,
        api_key: String::new(),
        uds_enabled: false,
        ..Default::default()
    };
    let se = build_server_engine(&cfg, true).expect("stub engine build");
    let engine = EngineHandle::new(Arc::new(se.engine));
    HttpState {
        engine,
        api_key: Arc::new("test-key".into()),
        metrics: fm_server::metrics::HttpMetrics::new(),
    }
}

async fn post(app: &axum::Router, path: &str, body: &str, auth: Option<&str>) -> (u16, String) {
    let mut b = Request::builder().method("POST").uri(path);
    if let Some(a) = auth {
        b = b.header("authorization", a);
    }
    let r = b
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let resp = app.clone().oneshot(r).await.unwrap();
    let code = resp.status().as_u16();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap();
    (code, String::from_utf8_lossy(&bytes).into_owned())
}

const COMMIT_BODY: &str = r#"{"jsonrpc":"2.0","method":"commit","params":{"session_id":"s1","interaction":{"id":"ix-it-1","session_id":"s1","turns":[{"turn_idx":0,"user_message":"rust sqlite btree memory","assistant_message":"indexed retrieval","tool_calls":[]}],"timestamp":1,"metadata":{}}},"id":1}"#;

#[tokio::test]
async fn http_commit_retrieve_consolidate_roundtrip() {
    // 真链路: commit → LocalStore insert_vector + Persist put → retrieve → consolidate
    let st = stub_state();
    let app = fm_server::http::app(st);

    // 1. commit (鉴权通过)
    let (code, body) = post(
        &app,
        "/v1/memory/commit",
        COMMIT_BODY,
        Some("Bearer test-key"),
    )
    .await;
    assert_eq!(code, 200, "commit body={body}");
    assert!(body.contains("result"), "commit body={body}");

    // 2. retrieve (走 StubEmbedder embed → LocalStore search_knn)
    let ret_body =
        r#"{"jsonrpc":"2.0","method":"retrieve","params":{"text":"rust sqlite","top_k":5},"id":2}"#;
    let (code, body) = post(
        &app,
        "/v1/memory/retrieve",
        ret_body,
        Some("Bearer test-key"),
    )
    .await;
    assert_eq!(code, 200, "retrieve body={body}");

    // 3. consolidate (空库 saga, 不 panic)
    let con_body = r#"{"jsonrpc":"2.0","method":"consolidate","params":{},"id":3}"#;
    let (code, body) = post(
        &app,
        "/v1/memory/consolidate",
        con_body,
        Some("Bearer test-key"),
    )
    .await;
    assert_eq!(code, 200, "consolidate body={body}");

    // 4. get (返回 null 或 item)
    let get_body = r#"{"jsonrpc":"2.0","method":"get","params":{"id":"nonexistent"},"id":4}"#;
    let (code, body) = post(&app, "/v1/memory/commit", get_body, Some("Bearer test-key")).await;
    assert_eq!(code, 200, "get body={body}");
}

#[tokio::test]
async fn http_auth_enforced_on_real_engine() {
    // 真 engine 路径鉴权: 无 token → 401
    let st = stub_state();
    let app = fm_server::http::app(st);
    let (code, _) = post(&app, "/v1/memory/commit", COMMIT_BODY, None).await;
    assert_eq!(code, 401);

    // 错 token → 401
    let (code, _) = post(&app, "/v1/memory/commit", COMMIT_BODY, Some("Bearer wrong")).await;
    assert_eq!(code, 401);
}

#[tokio::test]
async fn http_delete_without_confirm_rejected_on_real_engine() {
    // delete 无 confirm=true → -32602 invalid_params (B5 二次确认)。§2.9: 映射 HTTP 400。
    let st = stub_state();
    let app = fm_server::http::app(st);
    let body = r#"{"jsonrpc":"2.0","method":"delete","params":{"id":"m1"},"id":1}"#;
    let (code, body) = post(&app, "/v1/memory/delete", body, Some("Bearer test-key")).await;
    assert_eq!(code, 400);
    assert!(body.contains("-32602"), "delete body={body}");
}
