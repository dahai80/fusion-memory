//! M7 #48 / M8 #49 e2e: SKILL.md export, standard JSONL/Markdown export
//! (redact-forced), content revision history — over the real HTTP boundary.
//!
//! Acceptance gates (evolution plan §6 v1.8.0): export passes redact
//! assertion; agent-studio consumption e2e (the exported SKILL.md body parses
//! as the skills frontmatter contract); revision-history migration is covered
//! by fm-persist `schema_v9_revisions_migration`.

use std::sync::Arc;

use axum::body::Body;
use axum::http::Request;
use fm_server::http::HttpState;
use fm_server::{build_server_engine, EngineHandle};
use tempfile::tempdir;
use tower::ServiceExt;

fn state() -> (HttpState, Arc<fm_engine::MemoryEngine>) {
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
        default_tenant: Arc::new(String::new()),
        multi_tenant: false,
        acl: false,
        verifier: fm_server::identity::noop_verifier(),
    };
    (st, concrete)
}

async fn post(app: &axum::Router, path: &str, body: &str) -> (u16, String) {
    let r = Request::builder()
        .method("POST")
        .uri(path)
        .header("authorization", "Bearer test-key")
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

fn commit_body(ix: &str, text: &str, mt: &str) -> String {
    format!(
        r#"{{"jsonrpc":"2.0","method":"commit","params":{{"session_id":"s1","namespace":"local","namespace_key":"m1","interaction":{{"id":"{ix}","session_id":"s1","turns":[{{"turn_idx":0,"user_message":"{text}","assistant_message":"","tool_calls":[]}}],"timestamp":1,"metadata":{{}}}},"memory_type":"{mt}"}},"id":1}}"#
    )
}

#[tokio::test]
async fn export_jsonl_markdown_redact_forced() {
    let (st, _c) = state();
    let app = fm_server::http::app(st);

    // PII-laden commit (commit-path redact already scrubs; export re-scrubs — belt).
    let (code, body) = post(
        &app,
        "/v1/memory/commit",
        &commit_body(
            "ix-exp",
            "user phone is 13800138000 and ops@example.com needs a callback",
            "Episodic",
        ),
    )
    .await;
    assert_eq!(code, 200, "{body}");

    for (fmt, marker) in [
        ("jsonl", "[REDACTED:"),
        ("markdown", "# Fusion Memory Export"),
    ] {
        let (code, body) = post(
            &app,
            "/v1/memory/export",
            &format!(
                r#"{{"jsonrpc":"2.0","method":"export","params":{{"format":"{fmt}"}},"id":2}}"#
            ),
        )
        .await;
        assert_eq!(code, 200, "{fmt} body={body}");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        let export_body = v["result"]["body"].as_str().unwrap();
        assert!(export_body.contains(marker), "{fmt} shape: {export_body}");
        // defense line 2 assertion: no raw PII in any export body.
        assert!(!export_body.contains("13800138000"), "{fmt} leaks phone");
        assert!(
            !export_body.contains("ops@example.com"),
            "{fmt} leaks email"
        );
    }
}

#[tokio::test]
async fn export_skills_procedural_contract() {
    let (st, concrete) = state();
    let app = fm_server::http::app(st);

    // Procedural memory → SKILL.md doc. Inserted directly: commit classifies
    // via the extractor, so memory_type is forced here (unit under test is the
    // export path, not classification).
    let item = fm_core::MemoryItem::new_turn_skeleton(
        "skill-1".into(),
        "ix-skill".into(),
        0,
        "s".into(),
        String::new(),
        fm_core::MemoryType::Procedural,
        "rotate the deploy key then restart the runner fleet".into(),
        1000,
    );
    let vec = concrete
        .embedder()
        .embed("rotate the deploy key then restart the runner fleet")
        .await
        .unwrap();
    concrete
        .store()
        .insert_vector(fm_embed::vector_id_from_ulid("skill-1"), &vec)
        .unwrap();
    concrete.persist().put_memory(&item).unwrap();
    let (code, body) = post(
        &app,
        "/v1/memory/export_skills",
        r#"{"jsonrpc":"2.0","method":"export_skills","params":{},"id":2}"#,
    )
    .await;
    assert_eq!(code, 200, "{body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    let docs = v["result"].as_array().expect("skills array");
    assert_eq!(docs.len(), 1, "one procedural skill: {body}");
    let md = docs[0]["markdown"].as_str().unwrap();
    // agent-studio / fusion-harness consumption contract: frontmatter + body shape.
    assert!(md.starts_with("---\n"), "frontmatter first: {md}");
    assert!(md.contains("name: "), "frontmatter name");
    assert!(md.contains("description: "), "frontmatter description");
    assert!(md.contains("source: fusion-memory"), "source stamp");
    assert!(md.contains("license: Apache-2.0"), "clean license path");
    // non-procedural memories must not appear.
    let (code, body) = post(
        &app,
        "/v1/memory/commit",
        &commit_body(
            "ix-ep",
            "an episodic observation about deployment timing",
            "Episodic",
        ),
    )
    .await;
    assert_eq!(code, 200, "{body}");
    let (_, body) = post(
        &app,
        "/v1/memory/export_skills",
        r#"{"jsonrpc":"2.0","method":"export_skills","params":{},"id":3}"#,
    )
    .await;
    assert!(
        !body.contains("episodic observation"),
        "skills only from Procedural: {body}"
    );
}

#[tokio::test]
async fn revisions_route_history() {
    let (st, concrete) = state();
    let app = fm_server::http::app(st);

    let (code, body) = post(
        &app,
        "/v1/memory/commit",
        &commit_body("ix-rev", "first version of the runbook", "Episodic"),
    )
    .await;
    assert_eq!(code, 200, "{body}");

    // find the memory id
    let (_, body) = post(
        &app,
        "/v1/memory/list",
        r#"{"jsonrpc":"2.0","method":"list","params":{"limit":10},"id":2}"#,
    )
    .await;
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    let mem_id = v["result"][0]["id"].as_str().unwrap().to_string();

    // content change via direct engine write (merge write-back path)
    let mut item = concrete.persist().get_memory(&mem_id).unwrap().unwrap();
    item.content = "second version of the runbook".into();
    item.last_accessed_timestamp = 3000;
    concrete.persist().put_memory(&item).unwrap();

    let (code, body) = post(
        &app,
        "/v1/memory/revisions",
        &format!(r#"{{"jsonrpc":"2.0","method":"revisions","params":{{"id":"{mem_id}"}},"id":3}}"#),
    )
    .await;
    assert_eq!(code, 200, "{body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    let revs = v["result"].as_array().unwrap();
    assert_eq!(revs.len(), 1, "one revision after change: {body}");
    assert_eq!(
        revs[0]["content"].as_str().unwrap().trim_end(),
        "first version of the runbook",
        "revision stores the prior content"
    );

    // unknown id → 404
    let (code, _) = post(
        &app,
        "/v1/memory/revisions",
        r#"{"jsonrpc":"2.0","method":"revisions","params":{"id":"ghost"},"id":4}"#,
    )
    .await;
    assert_eq!(code, 404);
}
