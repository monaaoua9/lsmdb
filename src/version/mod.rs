mod edit;
mod set;

use std::{
    fs,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, OnceLock,
    },
};

use crate::{
    cache::BlockCache,
    error::Result,
    iter::{BoxIter, InternalIterator},
    metrics::Metrics,
    sstable::{Table, TableIterator},
    types::{cmp_ikey, make_ikey, user_key, Hit, SeqNo, ValueKind},
};

pub use edit::{NewFile, VersionEdit};
pub use set::VersionSet;

pub const NUM_LEVELS: usize = 7;

pub struct FileMeta {
    pub number: u64,
    pub size: u64,
    pub smallest: Vec<u8>,
    pub largest: Vec<u8>,
    path: PathBuf,
    verify: bool,
    cache: Option<Arc<BlockCache>>,
    table: OnceLock<Arc<Table>>,
    obsolete: AtomicBool,
}

impl FileMeta {
    fn new(path: PathBuf, verify: bool, cache: Option<Arc<BlockCache>>, f: NewFile) -> Self {
        FileMeta {
            number: f.number,
            size: f.size,
            smallest: f.smallest,
            largest: f.largest,
            path,
            verify,
            cache,
            table: OnceLock::new(),
            obsolete: AtomicBool::new(false),
        }
    }

    pub fn table(&self) -> Result<Arc<Table>> {
        if let Some(t) = self.table.get() {
            return Ok(t.clone());
        }
        let t = Table::open(&self.path, self.verify, self.cache.clone())?;
        // two threads may both open; the loser's mapping is just dropped
        Ok(self.table.get_or_init(|| t).clone())
    }

    fn mark_obsolete(&self) {
        self.obsolete.store(true, Ordering::Release);
    }

    pub fn smallest_user(&self) -> &[u8] {
        user_key(&self.smallest)
    }

    pub fn largest_user(&self) -> &[u8] {
        user_key(&self.largest)
    }
}

impl Drop for FileMeta {
    fn drop(&mut self) {
        if self.obsolete.load(Ordering::Acquire) {
            // unmap first, windows won't delete a mapped file. if something
            // still holds the mapping the delete fails and open() sweeps it later
            drop(self.table.take());
            let _ = fs::remove_file(&self.path);
        }
    }
}

pub struct Version {
    pub levels: Vec<Vec<Arc<FileMeta>>>,
}

impl Version {
    pub fn num_files(&self, level: usize) -> usize {
        self.levels[level].len()
    }

    pub fn level_bytes(&self, level: usize) -> u64 {
        self.levels[level].iter().map(|f| f.size).sum()
    }

    pub fn get(&self, ukey: &[u8], seq: SeqNo, m: &Metrics) -> Result<Option<Hit>> {
        let seek = make_ikey(ukey, seq, ValueKind::Put);

        // L0 files overlap and are kept newest-first
        for f in &self.levels[0] {
            if f.smallest_user() <= ukey && ukey <= f.largest_user() {
                if let Some(h) = f.table()?.get(ukey, &seek, m)? {
                    return Ok(Some(h));
                }
            }
        }
        for files in &self.levels[1..] {
            let i = files.partition_point(|f| cmp_ikey(&f.largest, &seek).is_lt());
            if let Some(f) = files.get(i) {
                if f.smallest_user() <= ukey {
                    if let Some(h) = f.table()?.get(ukey, &seek, m)? {
                        return Ok(Some(h));
                    }
                }
            }
        }
        Ok(None)
    }

    pub fn overlapping(&self, level: usize, lo: &[u8], hi: &[u8]) -> Vec<Arc<FileMeta>> {
        self.levels[level]
            .iter()
            .filter(|f| f.largest_user() >= lo && f.smallest_user() <= hi)
            .cloned()
            .collect()
    }

    pub fn key_may_exist_below(&self, level: usize, ukey: &[u8]) -> bool {
        self.levels[level + 1..]
            .iter()
            .flatten()
            .any(|f| f.smallest_user() <= ukey && ukey <= f.largest_user())
    }

    pub fn add_iterators(&self, out: &mut Vec<BoxIter>) -> Result<()> {
        for f in &self.levels[0] {
            out.push(Box::new(f.table()?.iter()));
        }
        for files in &self.levels[1..] {
            if !files.is_empty() {
                out.push(Box::new(LevelIterator::new(files.clone())));
            }
        }
        Ok(())
    }
}

/// walks a sorted, non-overlapping level one file at a time
pub struct LevelIterator {
    // field order matters: the table iterator must drop before the
    // FileMeta Arcs so a last-reference delete can unmap first
    cur: Option<TableIterator>,
    err: Option<crate::error::Error>,
    files: Vec<Arc<FileMeta>>,
    idx: usize,
}

impl LevelIterator {
    pub fn new(files: Vec<Arc<FileMeta>>) -> Self {
        LevelIterator { cur: None, err: None, files, idx: 0 }
    }

    fn open(&mut self, idx: usize) {
        self.idx = idx;
        self.cur = None;
        if idx >= self.files.len() {
            return;
        }
        match self.files[idx].table() {
            Ok(t) => self.cur = Some(t.iter()),
            Err(e) => self.err = Some(e),
        }
    }

    fn skip_forward(&mut self) {
        while self.err.is_none() && !self.cur.as_ref().is_some_and(|c| c.valid()) {
            if let Some(c) = &self.cur {
                if let Err(e) = c.status() {
                    self.err = Some(e);
                    return;
                }
            }
            if self.idx + 1 >= self.files.len() {
                self.cur = None;
                return;
            }
            self.open(self.idx + 1);
            if let Some(c) = &mut self.cur {
                c.seek_to_first();
            }
        }
    }
}

impl LevelIterator {
    fn skip_backward(&mut self) {
        while self.err.is_none() && !self.cur.as_ref().is_some_and(|c| c.valid()) {
            if let Some(c) = &self.cur {
                if let Err(e) = c.status() {
                    self.err = Some(e);
                    return;
                }
            }
            if self.idx == 0 {
                self.cur = None;
                return;
            }
            self.open(self.idx - 1);
            if let Some(c) = &mut self.cur {
                c.seek_to_last();
            }
        }
    }
}

impl InternalIterator for LevelIterator {
    fn valid(&self) -> bool {
        self.cur.as_ref().is_some_and(|c| c.valid())
    }

    fn seek_to_first(&mut self) {
        self.open(0);
        if let Some(c) = &mut self.cur {
            c.seek_to_first();
        }
        self.skip_forward();
    }

    fn seek_to_last(&mut self) {
        if self.files.is_empty() {
            self.cur = None;
            return;
        }
        self.open(self.files.len() - 1);
        if let Some(c) = &mut self.cur {
            c.seek_to_last();
        }
        self.skip_backward();
    }

    fn prev(&mut self) {
        if let Some(c) = &mut self.cur {
            c.prev();
        }
        self.skip_backward();
    }

    fn seek(&mut self, target: &[u8]) {
        let i = self.files.partition_point(|f| cmp_ikey(&f.largest, target).is_lt());
        self.open(i);
        if let Some(c) = &mut self.cur {
            c.seek(target);
        }
        self.skip_forward();
    }

    fn next(&mut self) {
        if let Some(c) = &mut self.cur {
            c.next();
        }
        self.skip_forward();
    }

    fn key(&self) -> &[u8] {
        self.cur.as_ref().unwrap().key()
    }

    fn value(&self) -> &[u8] {
        self.cur.as_ref().unwrap().value()
    }

    fn status(&self) -> Result<()> {
        match &self.err {
            Some(e) => Err(e.clone()),
            None => self.cur.as_ref().map_or(Ok(()), |c| c.status()),
        }
    }
}
