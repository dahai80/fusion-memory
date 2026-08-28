//! fm-persist: SQLite WAL 持久化层。PRD §4.3, §8.4。
//!
//! 单连接 + Mutex；WAL 模式（journal_mode=WAL, busy_timeout=5000, synchronous=NORMAL）。
//! MemoryItem 全字段 CRUD + consolidation/merge/wop 审计日志。

pub mod error;
pub mod schema;
pub mod store;

pub use error::{PersistError, PersistResult};
pub use store::{AuditLogEntry, MergeLogEntry, Persist, WopEntry};
