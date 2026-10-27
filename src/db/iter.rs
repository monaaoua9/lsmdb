use super::Snapshot;
use crate::{
    error::Result,
    iter::{BoxIter, InternalIterator, MergingIterator},
    types::{make_ikey, parse_ikey, SeqNo, ValueKind},
    version::Version,
};
use std::sync::Arc;

/// User-facing iterator: merges memtables and every sstable, hides
/// versions newer than its snapshot and collapses each key to its newest
/// visible value. Implements `Iterator<Item = Result<(key, value)>>`.
pub struct DbIter {
    // declaration order is drop order: sstable iterators go before the
    // version that keeps their files alive
    merge: MergingIterator,
    snapshot: Snapshot,
    upper: Option<Vec<u8>>,
    key: Vec<u8>,
    value: Vec<u8>,
    valid: bool,
    skip: Vec<u8>,
    skipping: bool,
    err_reported: bool,
    _version: Arc<Version>,
}

impl DbIter {
    pub(super) fn new(snapshot: Snapshot, lower: Option<&[u8]>, upper: Option<&[u8]>) -> Result<Self> {
        let (mem, imm, version) = {
            let s = snapshot.inner.state.lock().unwrap();
            (s.mem.clone(), s.imm.clone(), s.version.clone())
        };
        let mut children: Vec<BoxIter> = vec![Box::new(mem.iter())];
        if let Some(m) = imm {
            children.push(Box::new(m.iter()));
        }
        version.add_iterators(&mut children)?;

        let mut it = DbIter {
            merge: MergingIterator::new(children),
            snapshot,
            upper: upper.map(|u| u.to_vec()),
            key: Vec::new(),
            value: Vec::new(),
            valid: false,
            skip: Vec::new(),
            skipping: false,
            err_reported: false,
            _version: version,
        };
        match lower {
            Some(l) => it.merge.seek(&make_ikey(l, it.snapshot.seq, ValueKind::Put)),
            None => it.merge.seek_to_first(),
        }
        it.find_next();
        Ok(it)
    }

    pub fn valid(&self) -> bool {
        self.valid
    }

    pub fn key(&self) -> &[u8] {
        &self.key
    }

    pub fn value(&self) -> &[u8] {
        &self.value
    }

    pub fn seq(&self) -> SeqNo {
        self.snapshot.seq
    }

    pub fn status(&self) -> Result<()> {
        self.merge.status()
    }

    pub fn advance(&mut self) {
        if self.valid {
            self.merge.next();
            self.find_next();
        }
    }

    fn find_next(&mut self) {
        self.valid = false;
        while self.merge.valid() {
            let Some((uk, seq, kind)) = parse_ikey(self.merge.key()) else { return };
            if let Some(up) = &self.upper {
                if uk >= up.as_slice() {
                    return;
                }
            }
            if seq <= self.snapshot.seq {
                if self.skipping && uk == self.skip.as_slice() {
                    // older version of a key we already resolved
                } else {
                    self.skip.clear();
                    self.skip.extend_from_slice(uk);
                    self.skipping = true;
                    if kind == ValueKind::Put {
                        self.key.clear();
                        self.key.extend_from_slice(uk);
                        self.value.clear();
                        self.value.extend_from_slice(self.merge.value());
                        self.valid = true;
                        return;
                    }
                }
            }
            self.merge.next();
        }
    }
}

impl Iterator for DbIter {
    type Item = Result<(Vec<u8>, Vec<u8>)>;

    fn next(&mut self) -> Option<Self::Item> {
        if !self.valid {
            if !self.err_reported {
                self.err_reported = true;
                if let Err(e) = self.status() {
                    return Some(Err(e));
                }
            }
            return None;
        }
        let item = (self.key.clone(), self.value.clone());
        self.advance();
        Some(Ok(item))
    }
}
