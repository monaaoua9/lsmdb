use crate::{error::Result, types::cmp_ikey};
use std::cmp::Ordering;

/// forward-only iterator over internal keys. `status` reports I/O or
/// corruption errors that made the iterator stop early
pub trait InternalIterator {
    fn valid(&self) -> bool;
    fn seek_to_first(&mut self);
    fn seek(&mut self, target: &[u8]);
    fn next(&mut self);
    fn key(&self) -> &[u8];
    fn value(&self) -> &[u8];
    fn status(&self) -> Result<()>;
}

pub type BoxIter = Box<dyn InternalIterator + Send>;

// plain linear min-scan: fan-in is L0 files + one per deeper level, so ~15
// at the worst and a heap wouldn't win
pub struct MergingIterator {
    children: Vec<BoxIter>,
    cur: Option<usize>,
}

impl MergingIterator {
    pub fn new(children: Vec<BoxIter>) -> Self {
        MergingIterator { children, cur: None }
    }

    fn find_smallest(&mut self) {
        let mut best: Option<usize> = None;
        for (i, c) in self.children.iter().enumerate() {
            if !c.valid() {
                continue;
            }
            best = match best {
                Some(b) if cmp_ikey(c.key(), self.children[b].key()) != Ordering::Less => Some(b),
                _ => Some(i),
            };
        }
        self.cur = best;
    }
}

impl InternalIterator for MergingIterator {
    fn valid(&self) -> bool {
        self.cur.is_some()
    }

    fn seek_to_first(&mut self) {
        for c in &mut self.children {
            c.seek_to_first();
        }
        self.find_smallest();
    }

    fn seek(&mut self, target: &[u8]) {
        for c in &mut self.children {
            c.seek(target);
        }
        self.find_smallest();
    }

    fn next(&mut self) {
        if let Some(i) = self.cur {
            self.children[i].next();
            self.find_smallest();
        }
    }

    fn key(&self) -> &[u8] {
        self.children[self.cur.unwrap()].key()
    }

    fn value(&self) -> &[u8] {
        self.children[self.cur.unwrap()].value()
    }

    fn status(&self) -> Result<()> {
        for c in &self.children {
            c.status()?;
        }
        Ok(())
    }
}
