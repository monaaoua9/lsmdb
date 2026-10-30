use crate::{error::Result, types::cmp_ikey};
use std::cmp::Ordering;

/// iterator over internal keys that can move both ways. `status` reports I/O or
/// corruption errors that made the iterator stop early
pub trait InternalIterator {
    fn valid(&self) -> bool;
    fn seek_to_first(&mut self);
    fn seek_to_last(&mut self);
    fn seek(&mut self, target: &[u8]);
    fn next(&mut self);
    /// only called on a valid iterator; stepping off the front makes it invalid
    fn prev(&mut self);
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
    reverse: bool,
}

impl MergingIterator {
    pub fn new(children: Vec<BoxIter>) -> Self {
        MergingIterator { children, cur: None, reverse: false }
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

    fn find_largest(&mut self) {
        let mut best: Option<usize> = None;
        for (i, c) in self.children.iter().enumerate() {
            if !c.valid() {
                continue;
            }
            best = match best {
                Some(b) if cmp_ikey(c.key(), self.children[b].key()) != Ordering::Greater => Some(b),
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
        self.reverse = false;
        self.find_smallest();
    }

    fn seek_to_last(&mut self) {
        for c in &mut self.children {
            c.seek_to_last();
        }
        self.reverse = true;
        self.find_largest();
    }

    fn seek(&mut self, target: &[u8]) {
        for c in &mut self.children {
            c.seek(target);
        }
        self.reverse = false;
        self.find_smallest();
    }

    fn next(&mut self) {
        let Some(cur) = self.cur else { return };
        if self.reverse {
            // the other children sit before the current key; bring them to just after it
            let key = self.children[cur].key().to_vec();
            for (i, c) in self.children.iter_mut().enumerate() {
                if i == cur {
                    continue;
                }
                c.seek(&key);
                if c.valid() && cmp_ikey(c.key(), &key) == Ordering::Equal {
                    c.next();
                }
            }
            self.reverse = false;
        }
        self.children[cur].next();
        self.find_smallest();
    }

    fn prev(&mut self) {
        let Some(cur) = self.cur else { return };
        if !self.reverse {
            let key = self.children[cur].key().to_vec();
            for (i, c) in self.children.iter_mut().enumerate() {
                if i == cur {
                    continue;
                }
                c.seek(&key);
                if c.valid() {
                    c.prev();
                } else {
                    c.seek_to_last();
                }
            }
            self.reverse = true;
        }
        self.children[cur].prev();
        self.find_largest();
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
