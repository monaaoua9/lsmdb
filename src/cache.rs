use std::{
    collections::{BTreeMap, HashMap},
    sync::{
        atomic::{AtomicU64, Ordering::Relaxed},
        Arc, Mutex,
    },
};

const SHARDS: usize = 16;

type Key = (u64, u64);

struct Entry {
    data: Arc<Vec<u8>>,
    tick: u64,
}

#[derive(Default)]
struct Shard {
    map: HashMap<Key, Entry>,
    // tick -> key, oldest first
    lru: BTreeMap<u64, Key>,
    tick: u64,
    used: usize,
    cap: usize,
}

/// LRU cache of decompressed data blocks, keyed by (table id, block offset).
/// Raw blocks are served straight from the mmap and never land here.
pub struct BlockCache {
    shards: Vec<Mutex<Shard>>,
    hits: AtomicU64,
    misses: AtomicU64,
}

impl BlockCache {
    pub fn new(capacity: usize) -> BlockCache {
        let cap = capacity / SHARDS;
        let shards = (0..SHARDS).map(|_| Mutex::new(Shard { cap, ..Shard::default() })).collect();
        BlockCache { shards, hits: AtomicU64::new(0), misses: AtomicU64::new(0) }
    }

    fn shard(&self, table: u64, offset: u64) -> &Mutex<Shard> {
        let h = table.wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ offset.wrapping_mul(0xff51_afd7_ed55_8ccd);
        &self.shards[(h >> 32) as usize % SHARDS]
    }

    pub fn get(&self, table: u64, offset: u64) -> Option<Arc<Vec<u8>>> {
        let mut s = self.shard(table, offset).lock().unwrap();
        let key = (table, offset);
        s.tick += 1;
        let tick = s.tick;
        let Some(e) = s.map.get_mut(&key) else {
            self.misses.fetch_add(1, Relaxed);
            return None;
        };
        let old = std::mem::replace(&mut e.tick, tick);
        let data = e.data.clone();
        s.lru.remove(&old);
        s.lru.insert(tick, key);
        self.hits.fetch_add(1, Relaxed);
        Some(data)
    }

    pub fn insert(&self, table: u64, offset: u64, data: Arc<Vec<u8>>) {
        let mut s = self.shard(table, offset).lock().unwrap();
        if data.len() > s.cap {
            return;
        }
        let key = (table, offset);
        if let Some(old) = s.map.remove(&key) {
            s.lru.remove(&old.tick);
            s.used -= old.data.len();
        }
        while s.used + data.len() > s.cap {
            let Some((_, victim)) = s.lru.pop_first() else { break };
            if let Some(e) = s.map.remove(&victim) {
                s.used -= e.data.len();
            }
        }
        s.tick += 1;
        let tick = s.tick;
        s.used += data.len();
        s.lru.insert(tick, key);
        s.map.insert(key, Entry { data, tick });
    }

    pub fn hits(&self) -> u64 {
        self.hits.load(Relaxed)
    }

    pub fn misses(&self) -> u64 {
        self.misses.load(Relaxed)
    }

    pub fn used_bytes(&self) -> usize {
        self.shards.iter().map(|s| s.lock().unwrap().used).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blob(n: usize) -> Arc<Vec<u8>> {
        Arc::new(vec![7; n])
    }

    #[test]
    fn hit_and_miss() {
        let c = BlockCache::new(1 << 20);
        assert!(c.get(1, 0).is_none());
        c.insert(1, 0, blob(100));
        assert_eq!(c.get(1, 0).unwrap().len(), 100);
        assert!(c.get(2, 0).is_none());
        assert_eq!((c.hits(), c.misses()), (1, 2));
    }

    #[test]
    fn evicts_least_recently_used() {
        // every key below lands in the same shard by construction of the test
        let c = BlockCache::new(SHARDS * 300);
        let same: Vec<u64> =
            (0..1000u64).filter(|o| std::ptr::eq(c.shard(1, *o), c.shard(1, 0))).take(4).collect();
        c.insert(1, same[0], blob(100));
        c.insert(1, same[1], blob(100));
        c.insert(1, same[2], blob(100));
        // touch the oldest so same[1] becomes the victim
        assert!(c.get(1, same[0]).is_some());
        c.insert(1, same[3], blob(100));
        assert!(c.get(1, same[1]).is_none());
        assert!(c.get(1, same[0]).is_some());
        assert!(c.get(1, same[2]).is_some());
        assert!(c.get(1, same[3]).is_some());
    }

    #[test]
    fn oversized_block_is_skipped() {
        let c = BlockCache::new(SHARDS * 50);
        c.insert(1, 0, blob(51));
        assert!(c.get(1, 0).is_none());
        assert_eq!(c.used_bytes(), 0);
    }

    #[test]
    fn reinsert_replaces_accounting() {
        let c = BlockCache::new(1 << 20);
        c.insert(1, 0, blob(100));
        c.insert(1, 0, blob(40));
        assert_eq!(c.used_bytes(), 40);
    }
}
