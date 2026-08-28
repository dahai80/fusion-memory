//! persist 错误类型。对外经 `MemoryError::Sqlite`。

use thiserror::Error;

pub type PersistResult<T> = Result<T, PersistError>;

#[derive(Debug, Error)]
pub enum PersistError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("serde error: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    // P1: Mutex poison — 持锁线程 panic 后续不再 panic 放大, 上抛 Err 让调用方决策。
    #[error("persist conn lock poisoned (prior panic in critical section)")]
    Poisoned,

    // §1.1: r2d2 连接池错误 (池满超时/连接初始化失败)。
    #[error("connection pool error: {0}")]
    Pool(String),

    // v1.0.0 B-1: 静态加密错误 (key 缺失/格式错/解密失败)。
    #[error("encryption error: {0}")]
    Encrypt(String),
}

// §1.1: r2d2::Error → PersistError::Pool (供 Pool::builder().build()? 的 ? 转换)。
impl From<r2d2::Error> for PersistError {
    fn from(e: r2d2::Error) -> Self {
        PersistError::Pool(e.to_string())
    }
}

impl PersistError {
    pub fn to_memory(self) -> fm_core::MemoryError {
        match self {
            PersistError::Serde(e) => fm_core::MemoryError::Serde(e),
            PersistError::Io(e) => fm_core::MemoryError::Io(e),
            // §2.8: Poisoned → MemoryError::Poisoned (非 Sqlite 字符串)。
            // 旧版压成 Sqlite("...poisoned"), 运维误当 sqlite 错误跑 VACUUM, 真诊断 (重启) 被隐藏。
            PersistError::Poisoned => fm_core::MemoryError::Poisoned,
            // §1.1: 池错误 (满/超时/初始化) → Busy (瞬时可重试) 或 Sqlite。保守判字符串含 busy/timeout → Busy。
            PersistError::Pool(s) => {
                if s.to_lowercase().contains("timed out") || s.to_lowercase().contains("busy") {
                    fm_core::MemoryError::Busy(s)
                } else {
                    fm_core::MemoryError::Sqlite(s)
                }
            }
            // §2.8/§3.5: SQLITE_BUSY (SQLITE_BUSY/locked) → MemoryError::Busy (可重试)。
            // 旧版全压成 Sqlite 字符串, 调用方无法区分瞬时 vs 永久, 且 §3.5 被 .ok() 吞成全表扫。
            PersistError::Sqlite(e) => {
                if sqlite_is_busy(&e) {
                    fm_core::MemoryError::Busy(e.to_string())
                } else {
                    fm_core::MemoryError::Sqlite(e.to_string())
                }
            }
            // v1.0.0 B-1: 加密错误 → MemoryError::Encrypt (永久, 不可重试)。
            PersistError::Encrypt(s) => fm_core::MemoryError::Encrypt(s),
        }
    }
}

// SQLITE_BUSY (code 5) / SQLITE_LOCKED (code 6) → 瞬时可重试。其余按永久处理。
fn sqlite_is_busy(e: &rusqlite::Error) -> bool {
    if let Some(code) = e.sqlite_extended_error_code() {
        return code == 5 || code == 6;
    }
    e.to_string().to_lowercase().contains("busy")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serde_to_memory_passthrough() {
        let e: PersistError = serde_json::from_str::<serde_json::Value>("{bad}")
            .unwrap_err()
            .into();
        let m = e.to_memory();
        assert!(matches!(m, fm_core::MemoryError::Serde(_)));
    }

    #[test]
    fn io_to_memory_passthrough() {
        let e: PersistError = std::fs::read("/nonexistent-xyz-123").unwrap_err().into();
        let m = e.to_memory();
        assert!(matches!(m, fm_core::MemoryError::Io(_)));
    }

    #[test]
    fn sqlite_to_memory_string() {
        let e = PersistError::Sqlite(rusqlite::Error::InvalidQuery);
        let m = e.to_memory();
        assert!(matches!(m, fm_core::MemoryError::Sqlite(_)));
    }

    // §2.8: Poisoned → MemoryError::Poisoned (非 Sqlite 字符串)。运维可据此区分锁中毒 vs sqlite 错误。
    #[test]
    fn poisoned_to_memory_poisoned() {
        let m = PersistError::Poisoned.to_memory();
        assert!(matches!(m, fm_core::MemoryError::Poisoned));
        assert!(!m.retryable(), "Poisoned 永久, 不可重试");
    }

    // §2.8: Poisoned 非 NotFound (is_not_found=false)。
    #[test]
    fn poisoned_not_found_flag() {
        let m = PersistError::Poisoned.to_memory();
        assert!(!m.is_not_found());
    }

    // P1-9: pool get 超时 (GetTimeout, Display 含 "timed out") → MemoryError::Busy (可重试),
    // 非无限阻塞。模拟 r2d2 GetTimeout 的字符串映射。
    #[test]
    fn p1_9_pool_timeout_maps_to_busy() {
        let e = PersistError::Pool("timed out waiting for connection".into());
        let m = e.to_memory();
        assert!(matches!(m, fm_core::MemoryError::Busy(_)), "got {m:?}");
        assert!(m.retryable(), "Busy 可重试");
    }

    // P1-9: 真实触发 connection_timeout。1 连池 + 极短超时, 持有唯一连接后并发 get → 超时返 Busy。
    #[test]
    fn p1_9_pool_get_timeout_returns_busy_not_block() {
        use r2d2::Pool;
        use r2d2_sqlite::SqliteConnectionManager;
        use std::time::Duration;
        let mgr = SqliteConnectionManager::memory();
        let pool = Pool::builder()
            .max_size(1)
            .connection_timeout(Duration::from_millis(100))
            .build(mgr)
            .expect("pool");
        // 持有唯一连接, 让第二个 get 超时 (get() 用 configured connection_timeout)
        let _held = pool.get().expect("first conn");
        let t0 = std::time::Instant::now();
        let res = pool.get();
        let elapsed = t0.elapsed();
        assert!(res.is_err(), "second get must time out, got {res:?}");
        assert!(
            elapsed < Duration::from_secs(2),
            "must not block forever, elapsed {elapsed:?}"
        );
        // 映射到 MemoryError::Busy
        let pe: PersistError = res.unwrap_err().into();
        let m = pe.to_memory();
        assert!(matches!(m, fm_core::MemoryError::Busy(_)), "got {m:?}");
    }
}
