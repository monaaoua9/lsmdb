use super::Snapshot;
use crate::{
    error::Result,
    iter::{BoxIter, InternalIterator, MergingIterator},
    types::{make_ikey, parse_ikey, SeqNo, ValueKind, MAX_SEQ},
    version::Version,
};
use std::sync::Arc;

/// User-facing iterator: merges memtables and every sstable, hides
/// versions newer than its snapshot and collapses each key to its newest
/// visible value. Implements `Iterator<Item = Result<(key, value)>>`.
///
/// It moves both ways: `advance`/`retreat`, plus `seek`, `seek_to_last` and
/// `seek_for_prev`. `rev()` turns it into a descending `Iterator`.
pub struct DbIter {
    // declaration order is drop order: sstable iterators go before the
    // version that keeps their files alive
    merge: MergingIterator,
    snapshot: Snapshot,
    lower: Option<Vec<u8>>,
    upper: Option<Vec<u8>>,
    key: Vec<u8>,
    value: Vec<u8>,
    valid: bool,
    skip: Vec<u8>,
    skipping: bool,
    // forward: merge sits on the entry we returned. reverse: merge sits just
    // before every entry of the returned key
    reverse: bool,
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
            lower: lower.map(|l| l.to_vec()),
            upper: upper.map(|u| u.to_vec()),
            key: Vec::new(),
            value: Vec::new(),
            valid: false,
            skip: Vec::new(),
            skipping: false,
            reverse: false,
            err_reported: false,
            _version: version,
        };
        it.seek_to_first();
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

    pub fn seek_to_first(&mut self) {
        match self.lower.clone() {
            Some(l) => self.seek(&l),
            None => {
                self.merge.seek_to_first();
                self.reset_forward();
            }
        }
    }

    /// repositions at the first visible key >= `key` (never below the lower bound)
    pub fn seek(&mut self, key: &[u8]) {
        let key = match &self.lower {
            Some(l) if l.as_slice() > key => l.as_slice(),
            _ => key,
        };
        self.merge.seek(&make_ikey(key, self.snapshot.seq, ValueKind::Put));
        self.reset_forward();
    }

    pub fn seek_to_last(&mut self) {
        let target = self.upper.as_ref().map(|u| make_ikey(u, MAX_SEQ, ValueKind::Put));
        self.reverse_from(target);
    }

    /// repositions at the last visible key <= `key` (and below the upper bound)
    pub fn seek_for_prev(&mut self, key: &[u8]) {
        if self.upper.as_deref().is_some_and(|u| key >= u) {
            return self.seek_to_last();
        }
        // seq 0 is never assigned, so this sorts after every real entry of `key`
        self.reverse_from(Some(make_ikey(key, 0, ValueKind::Delete)));
    }

    pub fn advance(&mut self) {
        if !self.valid {
            return;
        }
        if self.reverse {
            self.reverse = false;
            if self.merge.valid() {
                self.merge.next();
            } else {
                self.merge.seek_to_first();
            }
            self.skip.clear();
            self.skip.extend_from_slice(&self.key);
            self.skipping = true;
        } else {
            self.merge.next();
        }
        self.find_next();
    }

    pub fn retreat(&mut self) {
        if !self.valid {
            return;
        }
        if !self.reverse {
            let cur = self.key.clone();
            loop {
                self.merge.prev();
                if !self.merge.valid() {
                    self.reverse = true;
                    self.valid = false;
                    return;
                }
                if parse_ikey(self.merge.key()).is_some_and(|(uk, _, _)| uk < cur.as_slice()) {
                    break;
                }
            }
            self.reverse = true;
        }
        self.find_prev();
    }

    /// consumes the iterator and walks the same range from the top down
    pub fn rev(mut self) -> RevIter {
        self.seek_to_last();
        RevIter(self)
    }

    fn reset_forward(&mut self) {
        self.reverse = false;
        self.skipping = false;
        self.find_next();
    }

    // `target` None = the very end. lands on the last internal entry before it
    fn reverse_from(&mut self, target: Option<Vec<u8>>) {
        match target {
            Some(t) => {
                self.merge.seek(&t);
                if self.merge.valid() {
                    self.merge.prev();
                } else {
                    self.merge.seek_to_last();
                }
            }
            None => self.merge.seek_to_last(),
        }
        self.reverse = true;
        self.find_prev();
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

    // Going backwards the versions of a key show up oldest first, so every
    // visible entry overwrites the last one; the previous key's group ending
    // (a smaller visible key) is what tells us we're done with this one.
    fn find_prev(&mut self) {
        let mut seen: Option<ValueKind> = None;
        while self.merge.valid() {
            let Some((uk, seq, kind)) = parse_ikey(self.merge.key()) else { break };
            if self.lower.as_deref().is_some_and(|l| uk < l) {
                break;
            }
            if seq <= self.snapshot.seq {
                if seen == Some(ValueKind::Put) && uk < self.key.as_slice() {
                    break;
                }
                seen = Some(kind);
                if kind == ValueKind::Put {
                    self.key.clear();
                    self.key.extend_from_slice(uk);
                    self.value.clear();
                    self.value.extend_from_slice(self.merge.value());
                } else {
                    self.key.clear();
                    self.value.clear();
                }
            }
            self.merge.prev();
        }
        self.valid = seen == Some(ValueKind::Put);
    }

    fn take_item(&mut self, step: fn(&mut Self)) -> Option<Result<(Vec<u8>, Vec<u8>)>> {
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
        step(self);
        Some(Ok(item))
    }
}

impl Iterator for DbIter {
    type Item = Result<(Vec<u8>, Vec<u8>)>;

    fn next(&mut self) -> Option<Self::Item> {
        self.take_item(DbIter::advance)
    }
}

/// descending view of a range, made by `DbIter::rev`
pub struct RevIter(DbIter);

impl Iterator for RevIter {
    type Item = Result<(Vec<u8>, Vec<u8>)>;

    fn next(&mut self) -> Option<Self::Item> {
        self.0.take_item(DbIter::retreat)
    }
}
