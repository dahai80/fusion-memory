//! M5 #46: per-memory ACL contract tests over the real HTTP boundary.
//!
//! Identity source = fusion-identity JWT (v1.3.0 boundary); tenant still only
//! trusts the gateway stamp (defense line 5). Owner bits come from the JWT sub
//! claim, stamped authoritatively at commit; reads (get/retrieve/list/provenance)
//! and delete are filtered fail-closed (deny = NotFound, no existence leak).
//!
//! Contract: acl=true requires multi_tenant=true (config validate); single-tenant
//! and UDS callers keep full access (None identity → no filtering, old behavior).

use std::sync::Arc;

use axum::body::Body;
use axum::http::Request;
use fm_server::http::HttpState;
use fm_server::identity::{CallerCtx, IdentityVerifier, VerifyResult};
use fm_server::{build_server_engine, EngineHandle};
use tempfile::tempdir;
use tower::ServiceExt;

/// Mock fusion-identity: token "Bearer <user>-jwt" → tid=acme, sub=<user>, role.
/// "admin-jwt" → role admin. Anything else → 401 (fail-closed).
struct MockIdentity;

#[async_trait::async_trait]
impl IdentityVerifier for MockIdentity {
    async fn verify(&self, jwt: &str) -> VerifyResult {
        let user = jwt.strip_suffix("-jwt");
        match user {
            Some("admin") => Ok(CallerCtx {
                tid: "acme".into(),
                sub: "admin".into(),
                role: "admin".into(),
            }),
            Some(u) => Ok(CallerCtx {
                tid: "acme".into(),
                sub: u.to_string(),
                role: String::new(),
            }),
            None => Err((
                axum::http::StatusCode::UNAUTHORIZED,
                axum::Json(serde_json::json!({"error": "invalid token"})),
            )),
        }
    }
    async fn report_usage(&self, _t: &str, _m: &str, _v: u64) {}
}

fn acl_state() -> (HttpState, Arc<fm_engine::MemoryEngine>) {
    let dir = tempdir().expect("tempdir");
    let cfg = fm_server::ServerConfig {
        data_dir: dir.path().to_path_buf(),
        http_port: 0,
        api_key: String::new(),
        uds_enabled: false,
        ..Default::default()
    };
    let se = build_server_engine(&cfg, true).expect("stub engine build");
    let concrete = Arc::new(se.engine);
    let engine = EngineHandle::new(concrete.clone());
    let st = HttpState {
        engine,
        concrete: Some(concrete.clone()),
        api_key: Arc::new("test-key".into()),
        metrics: fm_server::metrics::HttpMetrics::new(),
        gateway_origin_required: false,
        default_tenant: Arc::new("acme".into()),
        multi_tenant: true,
        acl: true,
        verifier: Arc::new(MockIdentity),
    };
    (st, concrete)
}

async fn post(app: &axum::Router, path: &str, body: &str, user: &str) -> (u16, String) {
    let mut b = Request::builder()
        .method("POST")
        .uri(path)
        .header("X-Fusion-Tenant", "acme")
        .header("authorization", format!("Bearer {user}-jwt"));
    if path != "/healthz" {
        b = b.header("content-type", "application/json");
    }
    let r = b.body(Body::from(body.to_string())).unwrap();
    let resp = app.clone().oneshot(r).await.unwrap();
    let code = resp.status().as_u16();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap();
    (code, String::from_utf8_lossy(&bytes).into_owned())
}

async fn get_mem(app: &axum::Router, id: &str, user: &str) -> (u16, String) {
    let r = Request::builder()
        .method("GET")
        .uri(format!("/v1/memory/{id}"))
        .header("X-Fusion-Tenant", "acme")
        .header("authorization", format!("Bearer {user}-jwt"))
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(r).await.unwrap();
    let code = resp.status().as_u16();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap();
    (code, String::from_utf8_lossy(&bytes).into_owned())
}

fn commit_body(ix: &str, text: &str) -> String {
    format!(
        r#"{{"jsonrpc":"2.0","method":"commit","params":{{"session_id":"s1","interaction":{{"id":"{ix}","session_id":"s1","turns":[{{"turn_idx":0,"user_message":"{text}","assistant_message":"","tool_calls":[]}}],"timestamp":1,"metadata":{{}}}}}},"id":1}}"#
    )
}

#[tokio::test]
async fn acl_owner_isolation_and_admin_bypass() {
    let (st, concrete) = acl_state();
    let app = fm_server::http::app(st);

    // alice commits → owner authoritatively stamped from JWT sub (client claim ignored).
    let (code, body) = post(
        &app,
        "/v1/memory/commit",
        &commit_body("ix-a", "rust sqlite btree memory"),
        "alice",
    )
    .await;
    assert_eq!(code, 200, "{body}");
    let (code, body) = post(
        &app,
        "/v1/memory/list",
        r#"{"jsonrpc":"2.0","method":"list","params":{"limit":10},"id":2}"#,
        "alice",
    )
    .await;
    assert_eq!(code, 200, "{body}");
    let items: serde_json::Value = serde_json::from_str(&body).unwrap();
    let mem_id = items["result"][0]["id"]
        .as_str()
        .expect("list result")
        .to_string();
    assert_eq!(
        items["result"][0]["owner"], "alice",
        "owner stamped from JWT sub"
    );

    // owner can read.
    let (code, body) = get_mem(&app, &mem_id, "alice").await;
    assert_eq!(code, 200, "{body}");

    // other user denied → 404 NotFound (no existence leak, fail-closed).
    let (code, body) = get_mem(&app, &mem_id, "bob").await;
    assert_eq!(code, 404, "bob must not see alice's memory: {body}");

    // admin bypasses owner check.
    let (code, _) = get_mem(&app, &mem_id, "admin").await;
    assert_eq!(code, 200, "admin bypass");

    // delete denial: bob 404, memory still there for alice.
    let (code, _) = post(&app, "/v1/memory/delete", &format!(r#"{{"jsonrpc":"2.0","method":"delete","params":{{"id":"{mem_id}","confirm":true}},"id":6}}"#), "bob").await;
    assert_eq!(code, 404, "bob must not delete alice's memory");
    let (_, body) = get_mem(&app, &mem_id, "alice").await;
    assert!(
        body.contains("runbook") || body.contains("memory"),
        "memory survived bob's delete attempt: {body}"
    );

    // owner can delete.
    let (code, _) = post(&app, "/v1/memory/delete", &format!(r#"{{"jsonrpc":"2.0","method":"delete","params":{{"id":"{mem_id}","confirm":true}},"id":8}}"#), "alice").await;
    assert_eq!(code, 200, "owner delete ok");

    // provenance on someone else's memory → 404.
    let (code, body) = post(
        &app,
        "/v1/memory/commit",
        &commit_body("ix-b", "kubernetes ingress rollout memory"),
        "alice",
    )
    .await;
    assert_eq!(code, 200, "{body}");
    let (_, body) = post(
        &app,
        "/v1/memory/list",
        r#"{"jsonrpc":"2.0","method":"list","params":{"limit":10},"id":10}"#,
        "alice",
    )
    .await;
    let items: serde_json::Value = serde_json::from_str(&body).unwrap();
    let mem_id2 = items["result"][0]["id"].as_str().unwrap().to_string();
    let (code, _) = post(
        &app,
        "/v1/memory/provenance",
        &format!(
            r#"{{"jsonrpc":"2.0","method":"provenance","params":{{"id":"{mem_id2}"}},"id":11}}"#
        ),
        "bob",
    )
    .await;
    assert_eq!(code, 404, "provenance must not leak alice's memory");
    let _ = concrete;
}

#[tokio::test]
async fn acl_multi_user_retrieve_isolation() {
    let (st, _c) = acl_state();
    let app = fm_server::http::app(st);

    // alice commits a distinctive memory.
    let (code, body) = post(
        &app,
        "/v1/memory/commit",
        &commit_body("ix-iso", "zephyr quantum ledger config"),
        "alice",
    )
    .await;
    assert_eq!(code, 200, "{body}");

    // alice retrieves → sees it; bob retrieves → nothing.
    let ret = r#"{"jsonrpc":"2.0","method":"retrieve","params":{"text":"zephyr quantum ledger","top_k":5},"id":2}"#;
    let (code, body) = post(&app, "/v1/memory/retrieve", ret, "alice").await;
    assert_eq!(code, 200, "{body}");
    assert!(
        body.contains("zephyr"),
        "owner must retrieve own memory: {body}"
    );
    let (code, body) = post(&app, "/v1/memory/retrieve", ret, "bob").await;
    assert_eq!(code, 200, "{body}");
    assert!(
        !body.contains("zephyr"),
        "other user must not retrieve alice's memory: {body}"
    );

    // list likewise.
    let lst = r#"{"jsonrpc":"2.0","method":"list","params":{"limit":10},"id":3}"#;
    let (_, body) = post(&app, "/v1/memory/list", lst, "alice").await;
    assert!(body.contains("zephyr"), "alice list sees own");
    let (_, body) = post(&app, "/v1/memory/list", lst, "bob").await;
    assert!(
        !body.contains("zephyr"),
        "bob list must not see alice's memory: {body}"
    );
}

#[tokio::test]
async fn acl_shared_memory_visible_to_all_users() {
    let (st, concrete) = acl_state();
    let app = fm_server::http::app(st);

    // Shared memory (owner=None) pre-inserted directly via the engine — legacy/shared rows stay visible to every user.
    let mut item = fm_core::MemoryItem::new_turn_skeleton(
        "shared-1".into(),
        "ix-shared".into(),
        0,
        "s".into(),
        "acme".into(),
        fm_core::MemoryType::Episodic,
        "deterministic shared runbook text".into(),
        1000,
    );
    let vec = concrete
        .embedder()
        .embed("deterministic shared runbook text")
        .await
        .unwrap();
    concrete
        .store()
        .insert_vector(fm_embed::vector_id_from_ulid("shared-1"), &vec)
        .unwrap();
    item.owner = None;
    concrete.persist().put_memory(&item).unwrap();

    for u in ["alice", "bob"] {
        let (code, body) = get_mem(&app, "shared-1", u).await;
        assert_eq!(code, 200, "{u} must see shared memory: {body}");
        assert!(body.contains("runbook"), "{u} shared content: {body}");
    }
}

#[tokio::test]
async fn acl_off_keeps_old_behavior() {
    let (mut st, _c) = acl_state();
    st.acl = false;
    let app = fm_server::http::app(st);

    let (code, body) = post(
        &app,
        "/v1/memory/commit",
        &commit_body("ix-c", "legacy no acl text"),
        "alice",
    )
    .await;
    assert_eq!(code, 200, "{body}");
    // acl=false → owner not stamped, all users see everything (backward compat).
    let (code, body) = post(
        &app,
        "/v1/memory/list",
        r#"{"jsonrpc":"2.0","method":"list","params":{"limit":10},"id":2}"#,
        "bob",
    )
    .await;
    assert_eq!(code, 200, "{body}");
    assert!(
        body.contains("legacy no acl text"),
        "acl off = no filtering: {body}"
    );
}
