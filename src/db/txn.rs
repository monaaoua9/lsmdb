use std::{
    collections::{BTreeMap, BTreeSet},
    iter::Peekable,
    ops::Bound,
    vec::IntoIter,
};

use super::DbIter;
use super::{
    write::{ConflictCheck, Writer},
    Db, Snapshot,
};
use crate::{batch::WriteBatch, error::Result};

/// Snapshot-isolation transaction. Reads see the database as of `begin()`
/// plus the transaction's own writes. `commit` is first-committer-wins: it
/// fails with `Error::Conflict` if any key it writes (or read through
/// `get_for_update`) was committed by someone else after the snapshot.
pub struct Transaction<'a> {
    db: &'a Db,
    snap: Snapshot,
    writes: BTreeMap<Vec<u8>, Option<Vec<u8>>>,
    guarded: BTreeSet<Vec<u8>>,
}

impl<'a> Transaction<'a> {
    pub(super) fn new(db: &'a Db) -> Self {
        Transaction { db, snap: db.snapshot(), writes: BTreeMap::new(), guarded: BTreeSet::new() }
    }

    pub fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>> {
        if let Some(v) = self.writes.get(key) {
            return Ok(v.clone());
        }
        self.snap.get(key)
    }

    /// like `get`, but a concurrent commit to `key` will make our commit fail;
    /// use it for read-modify-write and to close write-skew holes
    pub fn get_for_update(&mut self, key: &[u8]) -> Result<Option<Vec<u8>>> {
        self.guarded.insert(key.to_vec());
        self.get(key)
    }

    pub fn put(&mut self, key: impl Into<Vec<u8>>, value: impl Into<Vec<u8>>) {
        self.writes.insert(key.into(), Some(value.into()));
    }

    pub fn delete(&mut self, key: impl Into<Vec<u8>>) {
        self.writes.insert(key.into(), None);
    }

    /// scan that overlays this transaction's own writes on its snapshot
    pub fn range(&self, lower: Option<&[u8]>, upper: Option<&[u8]>) -> Result<TxnIter> {
        let base = self.db.inner.pin_snapshot(self.snap.seq).range(lower, upper)?;
        let lo = lower.map_or(Bound::Unbounded, Bound::Included);
        let hi = upper.map_or(Bound::Unbounded, Bound::Excluded);
        if let (Some(l), Some(u)) = (lower, upper) {
            if l >= u {
                return Ok(TxnIter { base, local: Vec::new().into_iter().peekable(), done: false });
            }
        }
        let local: Vec<_> =
            self.writes.range::<[u8], _>((lo, hi)).map(|(k, v)| (k.clone(), v.clone())).collect();
        Ok(TxnIter { base, local: local.into_iter().peekable(), done: false })
    }

    pub fn commit(self) -> Result<()> {
        if self.writes.is_empty() {
            return Ok(());
        }
        let mut keys: BTreeSet<Vec<u8>> = self.guarded;
        let mut batch = WriteBatch::new();
        for (k, v) in self.writes {
            keys.insert(k.clone());
            match v {
                Some(v) => batch.put(k, v),
                None => batch.delete(k),
            };
        }
        batch.validate()?;
        let check = ConflictCheck { start_seq: self.snap.seq, keys: keys.into_iter().collect() };
        // snapshot stays registered until after the commit lands, so the
        // tombstones/versions the conflict check looks at can't be compacted away
        let res = self.db.inner.submit(Writer::with_conflict(batch, true, check));
        drop(self.snap);
        res
    }

    pub fn rollback(self) {}
}

pub struct TxnIter {
    base: DbIter,
    local: Peekable<IntoIter<(Vec<u8>, Option<Vec<u8>>)>>,
    done: bool,
}

impl Iterator for TxnIter {
    type Item = Result<(Vec<u8>, Vec<u8>)>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let lk = self.local.peek().map(|(k, _)| k.as_slice());
            let bk = self.base.valid().then(|| self.base.key());
            // (take the local entry?, does it shadow the current base entry?)
            let (local_first, shadows) = match (bk, lk) {
                (None, None) => {
                    if self.done {
                        return None;
                    }
                    self.done = true;
                    return self.base.status().err().map(Err);
                }
                (Some(_), None) => (false, false),
                (None, Some(_)) => (true, false),
                (Some(b), Some(l)) => (l <= b, l == b),
            };
            if !local_first {
                let item = (self.base.key().to_vec(), self.base.value().to_vec());
                self.base.advance();
                return Some(Ok(item));
            }
            let (k, v) = self.local.next().unwrap();
            if shadows {
                self.base.advance();
            }
            // a local delete hides the key, keep looking
            if let Some(v) = v {
                return Some(Ok((k, v)));
            }
        }
    }
}
