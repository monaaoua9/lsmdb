use std::{
    cmp::Ordering::Less,
    ptr,
    sync::{
        atomic::{AtomicPtr, AtomicU64, AtomicUsize, Ordering::*},
        Arc,
    },
};

use crate::types::cmp_ikey;

const MAX_HEIGHT: usize = 12;

struct Node {
    key: Box<[u8]>,
    value: Box<[u8]>,
    next: Box<[AtomicPtr<Node>]>,
}

impl Node {
    fn alloc(key: Box<[u8]>, value: Box<[u8]>, height: usize) -> *mut Node {
        let next = (0..height).map(|_| AtomicPtr::new(ptr::null_mut())).collect();
        Box::into_raw(Box::new(Node { key, value, next }))
    }
}

/// Lock-free multi-writer, multi-reader skiplist. Nodes are only ever
/// linked in, never unlinked, so a pointer read through an Acquire load stays
/// valid until the list itself is dropped.
pub struct SkipList {
    head: *mut Node,
    height: AtomicUsize,
    rng: AtomicU64,
}

// SAFETY: all shared mutation goes through atomics, nodes are immutable
// after publication and freed only in Drop (which has &mut self)
unsafe impl Send for SkipList {}
unsafe impl Sync for SkipList {}

impl Default for SkipList {
    fn default() -> Self {
        Self::new()
    }
}

impl SkipList {
    pub fn new() -> Self {
        SkipList {
            head: Node::alloc(Box::new([]), Box::new([]), MAX_HEIGHT),
            height: AtomicUsize::new(1),
            rng: AtomicU64::new(0x2545_f491_4f6c_dd1d),
        }
    }

    fn random_height(&self) -> usize {
        const GAMMA: u64 = 0x9e37_79b9_7f4a_7c15;
        let mut z = self.rng.fetch_add(GAMMA, Relaxed).wrapping_add(GAMMA);
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^= z >> 31;
        let mut h = 1;
        while h < MAX_HEIGHT && z & 3 == 0 {
            h += 1;
            z >>= 2;
        }
        h
    }

    // walks right along `level` from `start`; returns (last node < key, first node >= key)
    fn scan(&self, start: *mut Node, level: usize, key: &[u8]) -> (*mut Node, *mut Node) {
        let mut p = start;
        loop {
            // SAFETY: p is head or a published node
            let n = unsafe { (*p).next[level].load(Acquire) };
            if !n.is_null() && unsafe { cmp_ikey(&(*n).key, key) } == Less {
                p = n;
            } else {
                return (p, n);
            }
        }
    }

    fn find_ge(&self, target: &[u8]) -> *mut Node {
        let mut x = self.head;
        let mut level = self.height.load(Acquire) - 1;
        loop {
            let (p, n) = self.scan(x, level, target);
            if level == 0 {
                return n;
            }
            x = p;
            level -= 1;
        }
    }

    pub fn insert(&self, key: Box<[u8]>, value: Box<[u8]>) {
        let h = self.random_height();
        let node = Node::alloc(key, value, h);
        // SAFETY: node was just allocated and isn't shared yet
        let key_ref: &[u8] = unsafe { &(*node).key };

        self.height.fetch_max(h, AcqRel);
        let top = self.height.load(Acquire).max(h);

        let mut prev = [ptr::null_mut::<Node>(); MAX_HEIGHT];
        let mut next = [ptr::null_mut::<Node>(); MAX_HEIGHT];
        let mut x = self.head;
        for level in (0..top).rev() {
            let (p, n) = self.scan(x, level, key_ref);
            prev[level] = p;
            next[level] = n;
            x = p;
        }

        // level 0 first: once linked there the entry is visible to readers
        for level in 0..h {
            loop {
                // SAFETY: prev[level] is head or published, node isn't published at this level yet
                unsafe {
                    (*node).next[level].store(next[level], Relaxed);
                    let cas = (*prev[level]).next[level].compare_exchange(next[level], node, AcqRel, Acquire);
                    if cas.is_ok() {
                        break;
                    }
                }
                // somebody slid in after prev[level]; rescan from there
                let (p, n) = self.scan(prev[level], level, key_ref);
                prev[level] = p;
                next[level] = n;
            }
        }
    }

    pub fn seek_ge(&self, target: &[u8]) -> Option<(&[u8], &[u8])> {
        let n = self.find_ge(target);
        if n.is_null() {
            None
        } else {
            // SAFETY: published node, lives as long as &self
            unsafe { Some((&(*n).key, &(*n).value)) }
        }
    }
}

impl Drop for SkipList {
    fn drop(&mut self) {
        // SAFETY: exclusive access; every node is reachable via level 0
        unsafe {
            let mut n = (*self.head).next[0].load(Relaxed);
            while !n.is_null() {
                let nx = (*n).next[0].load(Relaxed);
                drop(Box::from_raw(n));
                n = nx;
            }
            drop(Box::from_raw(self.head));
        }
    }
}

/// owns an Arc to the list so it can outlive the memtable handle that made it
pub struct SkipIter {
    list: Arc<SkipList>,
    node: *mut Node,
}

// SAFETY: only reads published, immutable nodes kept alive by `list`
unsafe impl Send for SkipIter {}

impl SkipIter {
    pub fn new(list: Arc<SkipList>) -> Self {
        SkipIter { list, node: ptr::null_mut() }
    }

    pub fn valid(&self) -> bool {
        !self.node.is_null()
    }

    pub fn seek_to_first(&mut self) {
        // SAFETY: head is always valid
        self.node = unsafe { (*self.list.head).next[0].load(Acquire) };
    }

    pub fn seek(&mut self, target: &[u8]) {
        self.node = self.list.find_ge(target);
    }

    pub fn next(&mut self) {
        debug_assert!(self.valid());
        // SAFETY: valid() checked by caller
        self.node = unsafe { (*self.node).next[0].load(Acquire) };
    }

    pub fn key(&self) -> &[u8] {
        debug_assert!(self.valid());
        unsafe { &(*self.node).key }
    }

    pub fn value(&self) -> &[u8] {
        debug_assert!(self.valid());
        unsafe { &(*self.node).value }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{make_ikey, ValueKind};
    use std::thread;

    fn ik(i: u32, seq: u64) -> Box<[u8]> {
        make_ikey(format!("k{i:06}").as_bytes(), seq, ValueKind::Put).into()
    }

    #[test]
    fn sorted_iteration() {
        let l = Arc::new(SkipList::new());
        for i in [5u32, 1, 9, 3, 7] {
            l.insert(ik(i, 1), Box::new([]));
        }
        let mut it = SkipIter::new(l);
        it.seek_to_first();
        let mut seen = Vec::new();
        while it.valid() {
            seen.push(it.key().to_vec());
            it.next();
        }
        let mut sorted = seen.clone();
        sorted.sort_by(|a, b| cmp_ikey(a, b));
        assert_eq!(seen, sorted);
        assert_eq!(seen.len(), 5);
    }

    #[test]
    fn seek_lands_on_first_ge() {
        let l = SkipList::new();
        l.insert(ik(2, 1), Box::new([]));
        l.insert(ik(4, 1), Box::new([]));
        let (k, _) = l.seek_ge(&ik(3, 1)).unwrap();
        assert_eq!(k, &ik(4, 1)[..]);
        assert!(l.seek_ge(&ik(5, 1)).is_none());
    }

    #[test]
    fn concurrent_inserts_lose_nothing() {
        let l = Arc::new(SkipList::new());
        let threads = 8u32;
        // miri is ~1000x slower
        let per = if cfg!(miri) { 20 } else { 5_000u32 };
        let hs: Vec<_> = (0..threads)
            .map(|t| {
                let l = l.clone();
                thread::spawn(move || {
                    for i in 0..per {
                        l.insert(ik(i * threads + t, 1), Box::new([]));
                    }
                })
            })
            .collect();
        for h in hs {
            h.join().unwrap();
        }
        let mut it = SkipIter::new(l);
        it.seek_to_first();
        let mut n = 0u32;
        let mut last: Option<Vec<u8>> = None;
        while it.valid() {
            if let Some(p) = &last {
                assert_eq!(cmp_ikey(p, it.key()), Less);
            }
            last = Some(it.key().to_vec());
            n += 1;
            it.next();
        }
        assert_eq!(n, threads * per);
    }

    #[test]
    fn readers_race_with_writer() {
        let l = Arc::new(SkipList::new());
        let n = if cfg!(miri) { 100 } else { 20_000 };
        let w = {
            let l = l.clone();
            thread::spawn(move || {
                for i in 0..n {
                    l.insert(ik(i, 1), Box::new([]));
                }
            })
        };
        let r = {
            let l = l.clone();
            thread::spawn(move || {
                for i in 0..n {
                    let _ = l.seek_ge(&ik(i, 1));
                }
            })
        };
        w.join().unwrap();
        r.join().unwrap();
    }
}
