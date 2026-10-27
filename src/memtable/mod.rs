mod skiplist;

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use crate::{
    error::Result,
    iter::InternalIterator,
    types::{make_ikey, parse_ikey, Hit, SeqNo, ValueKind},
};
use skiplist::{SkipIter, SkipList};

pub struct MemTable {
    list: Arc<SkipList>,
    bytes: AtomicUsize,
}

// rough per-node cost on top of the payload: node struct, tower, two boxes
const NODE_OVERHEAD: usize = 64;

impl MemTable {
    pub fn new() -> Self {
        MemTable { list: Arc::new(SkipList::new()), bytes: AtomicUsize::new(0) }
    }

    pub fn add(&self, seq: SeqNo, kind: ValueKind, ukey: &[u8], value: &[u8]) {
        let ik = make_ikey(ukey, seq, kind);
        self.bytes.fetch_add(ik.len() + value.len() + NODE_OVERHEAD, Ordering::Relaxed);
        self.list.insert(ik.into(), value.into());
    }

    pub fn get(&self, ukey: &[u8], seq: SeqNo) -> Option<Hit> {
        let target = make_ikey(ukey, seq, ValueKind::Put);
        let (k, v) = self.list.seek_ge(&target)?;
        let (u, s, kind) = parse_ikey(k)?;
        if u != ukey {
            return None;
        }
        let value = (kind == ValueKind::Put).then(|| v.to_vec());
        Some(Hit { seq: s, value })
    }

    pub fn approx_bytes(&self) -> usize {
        self.bytes.load(Ordering::Relaxed)
    }

    pub fn is_empty(&self) -> bool {
        self.approx_bytes() == 0
    }

    pub fn iter(&self) -> MemTableIter {
        MemTableIter(SkipIter::new(self.list.clone()))
    }
}

impl Default for MemTable {
    fn default() -> Self {
        Self::new()
    }
}

pub struct MemTableIter(SkipIter);

impl InternalIterator for MemTableIter {
    fn valid(&self) -> bool {
        self.0.valid()
    }

    fn seek_to_first(&mut self) {
        self.0.seek_to_first();
    }

    fn seek(&mut self, target: &[u8]) {
        self.0.seek(target);
    }

    fn next(&mut self) {
        self.0.next();
    }

    fn key(&self) -> &[u8] {
        self.0.key()
    }

    fn value(&self) -> &[u8] {
        self.0.value()
    }

    fn status(&self) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_reads_see_old_versions() {
        let m = MemTable::new();
        m.add(1, ValueKind::Put, b"k", b"v1");
        m.add(5, ValueKind::Put, b"k", b"v5");
        m.add(9, ValueKind::Delete, b"k", b"");
        assert_eq!(m.get(b"k", 0).map(|h| h.seq), None);
        assert_eq!(m.get(b"k", 1).unwrap().value.unwrap(), b"v1");
        assert_eq!(m.get(b"k", 8).unwrap().value.unwrap(), b"v5");
        assert!(m.get(b"k", 9).unwrap().value.is_none());
        assert!(m.get(b"other", 9).is_none());
    }

    #[test]
    fn iterator_outlives_table_handle() {
        let m = MemTable::new();
        m.add(1, ValueKind::Put, b"a", b"1");
        let mut it = m.iter();
        drop(m);
        it.seek_to_first();
        assert!(it.valid());
        assert_eq!(it.value(), b"1");
    }
}
