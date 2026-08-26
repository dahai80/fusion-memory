//! query embedding LRU + TTL 缓存。PRD §6.4 A3 修正。
//!
//! 同 query 重复检索不重打 mlx。缓存键 = hash(query_text), 容量 1024, TTL 1h。
//! 纯 Rust 实现 (无 moka 依赖, Rule 2 最小依赖)。
//! 线程安全: Mutex<HashMap> + 朴素 LRU (访问时间戳淘汰, 量 1024 够用, 不引侵入式链表)。

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

struct Entry {
    vec: Vec<f32>,
    born: Instant,
    last_used: Instant,
}

/// LRU + TTL 缓存。key=u64 hash。
pub struct LruCache {
    inner: Mutex<HashMap<u64, Entry>>,
    capacity: usize,
    ttl: Duration,
}

impl LruCache {
    pub fn new(capacity: usize, ttl_secs: u64) -> Self {
        Self {
            inner: Mutex::new(HashMap::with_capacity(capacity.min(4096))),
            capacity: capacity.max(1),
            ttl: Duration::from_secs(ttl_secs),
        }
    }

    /// 查询 (命中且未过期才返回), 命中刷新 last_used。
    pub fn get(&self, key: u64) -> Option<Vec<f32>> {
        let mut map = self.inner.lock().ok()?;
        let now = Instant::now();
        let entry = map.get_mut(&key)?;
        if now.duration_since(entry.born) > self.ttl {
            map.remove(&key);
            return None;
        }
        entry.last_used = now;
        Some(entry.vec.clone())
    }

    /// 写入。超容量淘汰最久未用。
    pub fn put(&self, key: u64, vec: Vec<f32>) {
        let mut map = match self.inner.lock() {
            Ok(m) => m,
            Err(_) => return,
        };
        let now = Instant::now();
        if map.len() >= self.capacity && !map.contains_key(&key) {
            // 淘汰 last_used 最小 (最久未用)
            if let Some(evict_key) = map.iter().min_by_key(|(_, e)| e.last_used).map(|(k, _)| *k) {
                map.remove(&evict_key);
            }
        }
        map.insert(
            key,
            Entry {
                vec,
                born: now,
                last_used: now,
            },
        );
    }

    /// 当前条数 (测试/可观测)。
    pub fn len(&self) -> usize {
        self.inner.lock().map(|m| m.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// FNV-1a 64-bit hash (与 stub embedding 同算法, 稳定无 std::hash 依赖)。
pub fn fnv1a_64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn put_get_hit() {
        let c = LruCache::new(4, 3600);
        c.put(1, vec![0.1, 0.2]);
        assert_eq!(c.len(), 1);
        let v = c.get(1).unwrap();
        assert_eq!(v, vec![0.1, 0.2]);
    }

    #[test]
    fn miss_returns_none() {
        let c = LruCache::new(4, 3600);
        assert!(c.get(99).is_none());
    }

    #[test]
    fn lru_eviction() {
        let c = LruCache::new(2, 3600);
        c.put(1, vec![1.0]);
        c.put(2, vec![2.0]);
        // 访问 1 → 2 变最久未用
        let _ = c.get(1);
        c.put(3, vec![3.0]); // 容量 2, 淘汰 2
        assert!(c.get(2).is_none());
        assert!(c.get(1).is_some());
        assert!(c.get(3).is_some());
        assert_eq!(c.len(), 2);
    }

    #[test]
    fn ttl_expiry() {
        let c = LruCache::new(4, 0); // TTL 0 → 立即过期
        c.put(1, vec![1.0]);
        std::thread::sleep(std::time::Duration::from_millis(5));
        assert!(c.get(1).is_none(), "should expire after TTL");
    }

    #[test]
    fn overwrite_same_key_no_grow() {
        let c = LruCache::new(2, 3600);
        c.put(1, vec![1.0]);
        c.put(1, vec![9.0]);
        assert_eq!(c.len(), 1);
        assert_eq!(c.get(1).unwrap(), vec![9.0]);
    }

    #[test]
    fn fnv_stable() {
        assert_eq!(fnv1a_64(b"hello"), fnv1a_64(b"hello"));
        assert_ne!(fnv1a_64(b"hello"), fnv1a_64(b"world"));
    }
}
