//! 引擎句柄：Arc<dyn FusionMemoryEngine>，服务层共享。PRD §11.2。
//!
//! 服务多请求并发，引擎需 Send+Sync（trait 已约束）。句柄 clone 廉价。

use std::sync::Arc;

use fm_core::FusionMemoryEngine;

/// 引擎句柄。
#[derive(Clone)]
pub struct EngineHandle {
    inner: Arc<dyn FusionMemoryEngine>,
}

impl EngineHandle {
    pub fn new(engine: Arc<dyn FusionMemoryEngine>) -> Self {
        Self { inner: engine }
    }

    pub fn from_concrete<E: FusionMemoryEngine + 'static>(engine: E) -> Self {
        Self {
            inner: Arc::new(engine),
        }
    }
}

impl std::ops::Deref for EngineHandle {
    type Target = dyn FusionMemoryEngine;
    fn deref(&self) -> &Self::Target {
        &*self.inner
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fm_core::{
        ConsolidationReport, FormattedContext, Interaction, MemoryId, MemoryItem, RetrieveQuery,
    };

    struct Dummy;
    #[async_trait::async_trait]
    impl fm_core::FusionMemoryEngine for Dummy {
        async fn commit_episodic_memory(
            &self,
            _s: &str,
            ix: &Interaction,
        ) -> fm_core::MemoryResult<Vec<MemoryId>> {
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
        async fn audit_memory_access(
            &self,
            _e: &[String],
        ) -> fm_core::MemoryResult<Vec<MemoryItem>> {
            Ok(vec![])
        }
    }

    #[tokio::test]
    async fn new_constructor_works() {
        let h = EngineHandle::new(Arc::new(Dummy) as Arc<dyn FusionMemoryEngine>);
        let ix = Interaction {
            id: "ix".into(),
            session_id: "s".into(),
            turns: vec![],
            timestamp: 0,
            metadata: Default::default(),
        };
        let ids = h.commit_episodic_memory("s", &ix).await.unwrap();
        assert!(ids.is_empty());
    }

    #[tokio::test]
    async fn from_concrete_works() {
        let h = EngineHandle::from_concrete(Dummy);
        let cloned = h.clone();
        assert!(cloned.get_memory("x").await.unwrap().is_none());
    }
}
