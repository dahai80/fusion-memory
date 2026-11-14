//! #64 T2: team namespace tier — key-isolated shared memory.
//! Acceptance: write with namespace="team" key=A; read with key=B returns nothing;
//! key=A returns it. Mirrors the #21 suite style (raw-handle offline setup).

use std::sync::Arc;

use fm_core::{MemoryItem, MemoryType, RetrieveQuery};
use fm_embed::StubEmbedder;
use fm_engine::MemoryEngine;
use fm_persist::Persist;
use fm_store::LocalStore;

fn team_item(id: &str, vec_id: u64, content: &str, key: &str, ts: u64) -> MemoryItem {
    let mut m = MemoryItem::new_turn_skeleton(
        id.to_string(),
        format!("ix-{id}"),
        0,
        format!("s-{id}"),
        String::new(),
        MemoryType::Episodic,
        content.to_string(),
        ts, // ms — engine decay math is ms-based
    );
    m.entities_pending = false;
    m = m.with_namespace(fm_core::namespace::NS_TEAM, key.to_string());
    m.vector_ref = vec_id.to_string();
    m
}

fn query(text: &str, key: &str) -> RetrieveQuery {
    let mut q = RetrieveQuery::new(text, 5, 512);
    q.namespace = fm_core::namespace::NS_TEAM.to_string();
    q.namespace_key = key.to_string();
    q
}

// #64 T2 acceptance: team key isolation through the engine retrieve path.
#[tokio::test]
async fn team_key_isolation_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(LocalStore::open(dir.path(), 8).unwrap());
    let persist = Arc::new(Persist::open_in_memory().unwrap());
    let embedder: Arc<dyn fm_embed::Embedder> = Arc::new(StubEmbedder::new(8));
    let base: u64 = 1_800_000_000_000;
    for (idx, (id, content, key)) in [
        ("t-alpha", "rotate the api key every quarter", "alpha"),
        ("t-beta", "refund policy covers thirty days", "beta"),
    ]
    .into_iter()
    .enumerate()
    {
        let item = team_item(id, (900 + idx) as u64, content, key, base + idx as u64);
        let vec = embedder.embed(content).await.unwrap();
        use fm_store::FusionStoreEngine as _;
        store.insert_vector((900 + idx) as u64, &vec).unwrap();
        persist.put_memory(&item).unwrap();
    }
    let engine = MemoryEngine::new(store, persist, embedder);

    // Same key sees the memory.
    let own =
        fm_core::FusionMemoryEngine::retrieve_context(&engine, &query("api key rotation", "alpha"))
            .await
            .unwrap();
    assert!(
        own.blocks
            .iter()
            .any(|b| b.turns_text.contains("rotate the api key")),
        "key=alpha must see its own team memory"
    );
    // Different key sees nothing from the tier.
    let other =
        fm_core::FusionMemoryEngine::retrieve_context(&engine, &query("api key rotation", "beta"))
            .await
            .unwrap();
    assert!(
        !other
            .blocks
            .iter()
            .any(|b| b.turns_text.contains("rotate the api key")),
        "key=beta must not see key=alpha team memory"
    );
}
