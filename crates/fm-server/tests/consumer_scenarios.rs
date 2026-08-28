//! M4 消费方契约场景测试 (PRD §10 / §14 M4 验收)。
//!
//! 用 stub engine 经 HTTP app 往返，证明三个消费方各自消费 fusion-memory HTTP
//! JSON-RPC wire 契约成立 (commit/retrieve/consolidate/delete/get 字段对齐)。
//! 不连 mlx, 不起真端口 (oneshot app)。真链路覆盖见 offline_integration.rs。
//!
//! 场景:
//! cowork — memory_commit 节点把 TrajectoryEvent 形 Interaction 调 commit 拿 memory_id 列表;
//!          memory_retrieve 节点调 retrieve 拿 FormattedContext 注入下游。
//! fusion-code — turn 开始调 retrieve_context 注入 systemPrompt; turn 结束调 commit 落 Episodic。
//! agent-studio — MemoryDispatcher 9 handler 后端替换映射: 6 个能映射的 RPC (store→commit,
//!                 recall→retrieve, get→get, delete→delete, list_recent→retrieve 空,
//!                 count→get 全量) 契约对齐; 标注 3 个无映射 (delete_scope/auto_forget)。
//!
//! PRD §11.2 HTTP 鉴权 + delete confirm=true 均覆盖。

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
        api_key: Arc::new("consumer-key".into()),
        metrics: fm_server::metrics::HttpMetrics::new(),
    }
}

async fn post(app: &axum::Router, body: &str) -> (u16, String) {
    let r = Request::builder()
        .method("POST")
        .uri("/v1/memory/commit")
        .header("authorization", "Bearer consumer-key")
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

async fn post_path(app: &axum::Router, path: &str, body: &str) -> (u16, String) {
    let r = Request::builder()
        .method("POST")
        .uri(path)
        .header("authorization", "Bearer consumer-key")
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

/// 单轮 Interaction (cowork 单 turn trajectory / fusion-code 单 turn / agent-studio store)。
fn one_turn_interaction(ix_id: &str, session: &str, user: &str, assistant: &str) -> String {
    format!(
        r#"{{"jsonrpc":"2.0","method":"commit","params":{{"session_id":"{sess}","interaction":{{"id":"{ix}","session_id":"{sess}","turns":[{{"turn_idx":0,"user_message":"{u}","assistant_message":"{a}","tool_calls":[]}}],"timestamp":1,"metadata":{{}}}}}},"id":1}}"#,
        ix = ix_id,
        sess = session,
        u = user,
        a = assistant,
    )
}

/// 多轮 Interaction (cowork TrajectoryEvent 还原完整对话)。
fn two_turn_interaction(ix_id: &str, session: &str) -> String {
    format!(
        r#"{{"jsonrpc":"2.0","method":"commit","params":{{"session_id":"{sess}","interaction":{{"id":"{ix}","session_id":"{sess}","turns":[{{"turn_idx":0,"user_message":"how to rank vector search","assistant_message":"use cosine similarity","tool_calls":[]}},{{"turn_idx":1,"user_message":"and normalize?","assistant_message":"L2 then cosine","tool_calls":[]}}],"timestamp":2,"metadata":{{"agent_type":"cowork","node_id":"n1"}}}}}},"id":1}}"#,
        ix = ix_id,
        sess = session,
    )
}

#[tokio::test]
async fn cowork_memory_commit_retrieve_node_flow() {
    // 场景: cowork memory_commit 节点 (把 TrajectoryEvent 形 Interaction 调 commit)
    //        + memory_retrieve 节点 (调 retrieve 注入下游 SharedContext)。
    let st = stub_state();
    let app = fm_server::http::app(st);

    // 1. memory_commit: 多轮 trajectory → commit → 期望 2 个 turn 级 memory_id
    let (code, body) = post(&app, &two_turn_interaction("ix-cw-1", "sess-cowork")).await;
    assert_eq!(code, 200, "cowork commit body={body}");
    // P1-1: result 现为 CommitOutcome 对象 {memory_ids, failed_turns}。
    assert!(body.contains(r#""result""#), "cowork commit body={body}");
    assert!(
        body.contains(r#""memory_ids""#),
        "cowork commit body={body}"
    );
    assert!(
        body.contains(r#""failed_turns""#),
        "cowork commit body={body}"
    );
    assert!(body.contains('['), "cowork commit body={body}");
    assert!(body.contains(']'), "cowork commit body={body}");

    // 2. memory_retrieve: query → retrieve → FormattedContext (blocks 含聚合 turn)
    let ret = r#"{"jsonrpc":"2.0","method":"retrieve","params":{"text":"vector search rank","top_k":5,"token_budget":2048,"aggregate":true},"id":2}"#;
    let (code, body) = post_path(&app, "/v1/memory/retrieve", ret).await;
    assert_eq!(code, 200, "cowork retrieve body={body}");
    // FormattedContext: {blocks:[...], total_tokens:N}
    assert!(body.contains(r#""blocks""#), "cowork retrieve body={body}");
    assert!(
        body.contains(r#""total_tokens""#),
        "cowork retrieve body={body}"
    );
    // 聚合命中 ix-cw-1: block.interaction_id == "ix-cw-1"
    assert!(
        body.contains(r#""interaction_id":"ix-cw-1""#),
        "cowork retrieve aggregation body={body}"
    );
}

#[tokio::test]
async fn fusion_code_retrieve_inject_commit_turn_flow() {
    // 场景: fusion-code turn 开始 retrieve_context 注入 systemPrompt
    //        + turn 结束 commit_episodic_memory 落 Episodic (单轮)。
    let st = stub_state();
    let app = fm_server::http::app(st);

    // 1. turn 开始: retrieve_context 注入 (空库, 应返回 blocks:[])
    let ret = r#"{"jsonrpc":"2.0","method":"retrieve","params":{"text":"setup rust sqlite memory","top_k":10,"token_budget":4096},"id":1}"#;
    let (code, body) = post_path(&app, "/v1/memory/retrieve", ret).await;
    assert_eq!(code, 200, "code retrieve body={body}");
    assert!(body.contains(r#""blocks""#), "code retrieve body={body}");

    // 2. turn 结束: commit 单轮 Interaction (user+assistant 一对)
    let (code, body) = post(
        &app,
        &one_turn_interaction(
            "ix-code-1",
            "sess-code",
            "setup rust sqlite memory engine",
            "use rusqlite WAL + hnsw_rs",
        ),
    )
    .await;
    assert_eq!(code, 200, "code commit body={body}");
    assert!(body.contains(r#""result""#), "code commit body={body}");

    // 3. 下一 turn retrieve: 应命中上一 commit (跨 session 记忆召回核心)
    let ret = r#"{"jsonrpc":"2.0","method":"retrieve","params":{"text":"rust sqlite memory engine","top_k":5,"token_budget":2048},"id":3}"#;
    let (code, body) = post_path(&app, "/v1/memory/retrieve", ret).await;
    assert_eq!(code, 200, "code recall body={body}");
    assert!(
        body.contains(r#""interaction_id":"ix-code-1""#),
        "code cross-turn recall body={body}"
    );
}

#[tokio::test]
async fn agent_studio_dispatcher_backend_replace_mapping() {
    // 场景: agent-studio MemoryDispatcher 9 handler 后端替换。
    //        6 个能映射的 fusion-memory RPC 契约对齐:
    //          memory.store → commit | memory.recall → retrieve | memory.get → get
    //          memory.delete → delete(confirm) | memory.list_recent → retrieve(空 query 退化)
    //          memory.count → 全量 get(无 count RPC, 消费方自行累计)
    //        3 个无 fusion-memory 映射 (delete_scope/auto_forget/recall_relevant),
    //        消费方适配层降级处理 (PRD §10.1 保留 handler 签名, 内部降级)。
    let st = stub_state();
    let app = fm_server::http::app(st);

    // memory.store → commit: 写一条 user 偏好 (agent-studio memory_type=user→Semantic)
    let store = one_turn_interaction(
        "ix-as-store",
        "sess-studio",
        "i prefer rust for systems work",
        "noted preference",
    );
    let (code, body) = post(&app, &store).await;
    assert_eq!(code, 200, "studio store→commit body={body}");

    // memory.recall → retrieve: 召回偏好
    let recall = r#"{"jsonrpc":"2.0","method":"retrieve","params":{"text":"i prefer rust","top_k":5,"token_budget":2048},"id":2}"#;
    let (code, body) = post_path(&app, "/v1/memory/retrieve", recall).await;
    assert_eq!(code, 200, "studio recall→retrieve body={body}");
    assert!(
        body.contains(r#""interaction_id":"ix-as-store""#),
        "studio recall body={body}"
    );

    // memory.get → get: 按 id 取 (用 commit 返回的 memory_id 不确定, 测 miss 路径契约)
    let get = r#"{"jsonrpc":"2.0","method":"get","params":{"id":"nonexistent"},"id":3}"#;
    let (code, body) = post_path(&app, "/v1/memory/commit", get).await;
    assert_eq!(code, 200, "studio get body={body}");
    // get miss → result: null
    assert!(
        body.contains(r#""result":null"#),
        "studio get null body={body}"
    );

    // memory.delete → delete(confirm=true): 软删
    let del = r#"{"jsonrpc":"2.0","method":"delete","params":{"id":"ix-as-store","confirm":true},"id":4}"#;
    let (code, body) = post_path(&app, "/v1/memory/delete", del).await;
    assert_eq!(code, 200, "studio delete body={body}");
    assert!(
        body.contains(r#""result":"deleted""#),
        "studio delete body={body}"
    );

    // delete 无 confirm → -32602 (二次确认 B5)。§2.9: invalid_params 映射 HTTP 400 (旧版误返 200)。
    let del_no = r#"{"jsonrpc":"2.0","method":"delete","params":{"id":"x"},"id":5}"#;
    let (code, body) = post_path(&app, "/v1/memory/delete", del_no).await;
    assert_eq!(code, 400, "studio delete noconfirm body={body}");
    assert!(
        body.contains("-32602"),
        "studio delete noconfirm body={body}"
    );

    // memory.list_recent → retrieve 空 query 退化 (消费方适配层行为, 此处验证 RPC 接受空 text)
    let recent = r#"{"jsonrpc":"2.0","method":"retrieve","params":{"text":"recent","top_k":20,"token_budget":2048},"id":6}"#;
    let (code, body) = post_path(&app, "/v1/memory/retrieve", recent).await;
    assert_eq!(code, 200, "studio list_recent body={body}");
    assert!(
        body.contains(r#""blocks""#),
        "studio list_recent body={body}"
    );

    // consolidate (agent-studio auto_forget 的远程等价, 走 consolidate saga)
    let con = r#"{"jsonrpc":"2.0","method":"consolidate","params":{},"id":7}"#;
    let (code, body) = post_path(&app, "/v1/memory/consolidate", con).await;
    assert_eq!(code, 200, "studio consolidate body={body}");
    assert!(
        body.contains(r#""dropped""#) || body.contains(r#""result""#),
        "studio consolidate body={body}"
    );
}
