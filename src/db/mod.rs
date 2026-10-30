mod background;
mod iter;
mod txn;
mod write;

use std::{
    collections::{BTreeMap, VecDeque},
    fs::{self, File},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Condvar, Mutex,
    },
    thread::JoinHandle,
};

use crate::{
    batch::{replay_record, WriteBatch},
    error::{Error, Result},
    filename::{self, FileKind},
    iter::InternalIterator,
    memtable::MemTable,
    metrics::Metrics,
    options::{Options, WriteOptions},
    types::{Hit, SeqNo, MAX_SEQ},
    version::{NewFile, Version, VersionEdit, VersionSet, NUM_LEVELS},
    wal::{LogReader, LogWriter},
};

pub use iter::{DbIter, RevIter};
pub use txn::{Transaction, TxnIter};
use write::Writer;

struct State {
    mem: Arc<MemTable>,
    imm: Option<Arc<MemTable>>,
    // WALs older than this are dead once `imm` is flushed
    imm_next_log: u64,
    version: Arc<Version>,
}

pub(crate) struct DbInner {
    dir: PathBuf,
    opts: Options,
    // lock order: versions -> state. Readers only ever take `state`.
    state: Mutex<State>,
    state_cv: Condvar,
    bg_cv: Condvar,
    versions: Mutex<VersionSet>,
    wal: Mutex<LogWriter>,
    last_seq: AtomicU64,
    next_file: Arc<AtomicU64>,
    snapshots: Mutex<BTreeMap<SeqNo, usize>>,
    queue: Mutex<VecDeque<Arc<Writer>>>,
    queue_cv: Condvar,
    compaction_lock: Mutex<()>,
    shutdown: AtomicBool,
    bg_error: Mutex<Option<Error>>,
    metrics: Metrics,
    lock_file: Mutex<Option<File>>,
}

pub struct Db {
    inner: Arc<DbInner>,
    bg: Mutex<Option<JoinHandle<()>>>,
}

/// pinned read view; versions visible to it survive compaction until it drops
pub struct Snapshot {
    inner: Arc<DbInner>,
    seq: SeqNo,
}

#[derive(Debug, Clone, Default)]
pub struct Stats {
    pub files_per_level: Vec<usize>,
    pub bytes_per_level: Vec<u64>,
    pub memtable_bytes: usize,
    pub last_seq: SeqNo,
    pub bloom_checked: u64,
    pub bloom_skipped: u64,
    pub block_reads: u64,
    pub write_ops: u64,
    /// user payload (keys + values) accepted so far
    pub write_bytes: u64,
    pub wal_syncs: u64,
    pub conflicts: u64,
    /// writes that were slowed or blocked by too many L0 files
    pub stalls: u64,
    pub flushes: u64,
    pub flush_bytes: u64,
    pub compactions: u64,
    pub trivial_moves: u64,
    pub compaction_read_bytes: u64,
    pub compaction_write_bytes: u64,
}

impl Stats {
    /// bytes written to tables per user byte; 0 until something was written
    pub fn write_amplification(&self) -> f64 {
        if self.write_bytes == 0 {
            return 0.0;
        }
        (self.flush_bytes + self.compaction_write_bytes) as f64 / self.write_bytes as f64
    }
}

impl Db {
    pub fn open(path: impl AsRef<Path>, opts: Options) -> Result<Db> {
        opts.validate()?;
        let dir = path.as_ref().to_path_buf();
        if opts.create_if_missing {
            fs::create_dir_all(&dir)?;
        }
        let lock = File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(filename::lock_path(&dir))?;
        lock.try_lock().map_err(|_| Error::Locked)?;

        let next_file = Arc::new(AtomicU64::new(2));
        // the manifest's counter can lag behind files a crashed run created afterwards
        // (rotated wal, half-written table); never reuse a number that is already on disk
        for e in fs::read_dir(&dir)?.filter_map(|e| e.ok()) {
            match filename::parse(&e.file_name().to_string_lossy()) {
                FileKind::Log(n) | FileKind::Table(n) | FileKind::Manifest(n) => {
                    next_file.fetch_max(n + 1, Ordering::SeqCst);
                }
                _ => {}
            }
        }
        let mut vs = VersionSet::open(&dir, &opts, next_file.clone())?;

        let mut logs: Vec<u64> = fs::read_dir(&dir)?
            .filter_map(|e| e.ok())
            .filter_map(|e| match filename::parse(&e.file_name().to_string_lossy()) {
                FileKind::Log(n) if n >= vs.log_number() => Some(n),
                _ => None,
            })
            .collect();
        logs.sort_unstable();

        let mem = MemTable::new();
        let mut max_seq = vs.last_seq();
        for n in logs {
            next_file.fetch_max(n + 1, Ordering::SeqCst);
            let mut r = LogReader::open(&filename::log_path(&dir, n))?;
            while let Some(rec) = r.read_record() {
                let last = replay_record(rec, |seq, kind, k, v| mem.add(seq, kind, k, v))?;
                max_seq = max_seq.max(last);
            }
        }

        // recovered writes go straight to an L0 table so the old logs can be dropped
        let wal_number = next_file.fetch_add(1, Ordering::SeqCst);
        let mut edit = VersionEdit { log_number: Some(wal_number), ..Default::default() };
        if !mem.is_empty() {
            let number = next_file.fetch_add(1, Ordering::SeqCst);
            let mut it = mem.iter();
            it.seek_to_first();
            let info = background::write_table(&dir, &opts, number, &mut it)?;
            edit.added.push((
                0,
                NewFile { number, size: info.size, smallest: info.smallest, largest: info.largest },
            ));
        }
        let version = vs.log_and_apply(edit, max_seq)?;
        let wal = LogWriter::create(&filename::log_path(&dir, wal_number))?;

        background::sweep_obsolete(&dir, &vs);

        let inner = Arc::new(DbInner {
            dir,
            opts,
            state: Mutex::new(State { mem: Arc::new(MemTable::new()), imm: None, imm_next_log: 0, version }),
            state_cv: Condvar::new(),
            bg_cv: Condvar::new(),
            versions: Mutex::new(vs),
            wal: Mutex::new(wal),
            last_seq: AtomicU64::new(max_seq),
            next_file,
            snapshots: Mutex::new(BTreeMap::new()),
            queue: Mutex::new(VecDeque::new()),
            queue_cv: Condvar::new(),
            compaction_lock: Mutex::new(()),
            shutdown: AtomicBool::new(false),
            bg_error: Mutex::new(None),
            metrics: Metrics::default(),
            lock_file: Mutex::new(Some(lock)),
        });

        let bg = {
            let inner = inner.clone();
            std::thread::Builder::new()
                .name("lsmdb-bg".into())
                .spawn(move || inner.bg_loop())
                .map_err(Error::from)?
        };
        Ok(Db { inner, bg: Mutex::new(Some(bg)) })
    }

    pub fn put(&self, key: impl Into<Vec<u8>>, value: impl Into<Vec<u8>>) -> Result<()> {
        let mut b = WriteBatch::new();
        b.put(key, value);
        self.write(b, &WriteOptions::default())
    }

    pub fn delete(&self, key: impl Into<Vec<u8>>) -> Result<()> {
        let mut b = WriteBatch::new();
        b.delete(key);
        self.write(b, &WriteOptions::default())
    }

    pub fn write(&self, batch: WriteBatch, wo: &WriteOptions) -> Result<()> {
        batch.validate()?;
        if batch.is_empty() {
            return Ok(());
        }
        self.inner.submit(Writer::new(batch, wo.sync))
    }

    /// deletes every key in [lower, upper) as of now and returns how many.
    /// done in chunks of tombstones, so it is not atomic as a whole; keys
    /// written after the call starts are left alone
    pub fn delete_range(&self, lower: &[u8], upper: &[u8]) -> Result<u64> {
        self.delete_where(self.range(Some(lower), Some(upper))?)
    }

    pub fn delete_prefix(&self, prefix: &[u8]) -> Result<u64> {
        self.delete_where(self.prefix(prefix)?)
    }

    fn delete_where(&self, mut it: DbIter) -> Result<u64> {
        const CHUNK: usize = 1000;
        let (mut total, mut b) = (0u64, WriteBatch::new());
        while it.valid() {
            b.delete(it.key());
            total += 1;
            if b.len() >= CHUNK {
                self.write(std::mem::take(&mut b), &WriteOptions::default())?;
            }
            it.advance();
        }
        it.status()?;
        self.write(b, &WriteOptions::default())?;
        Ok(total)
    }

    pub fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>> {
        let seq = self.inner.last_seq.load(Ordering::Acquire);
        Ok(self.inner.get_at(key, seq)?.and_then(|h| h.value))
    }

    pub fn contains_key(&self, key: &[u8]) -> Result<bool> {
        Ok(self.get(key)?.is_some())
    }

    /// all keys are read at one sequence number, so the answers are mutually consistent
    pub fn get_many(&self, keys: &[&[u8]]) -> Result<Vec<Option<Vec<u8>>>> {
        let seq = self.inner.last_seq.load(Ordering::Acquire);
        keys.iter().map(|k| Ok(self.inner.get_at(k, seq)?.and_then(|h| h.value))).collect()
    }

    pub fn snapshot(&self) -> Snapshot {
        self.inner.acquire_snapshot()
    }

    pub fn path(&self) -> &Path {
        &self.inner.dir
    }

    pub fn options(&self) -> &Options {
        &self.inner.opts
    }

    /// full scan at the current sequence number
    pub fn iter(&self) -> Result<DbIter> {
        self.range(None, None)
    }

    /// keys in [lower, upper)
    pub fn range(&self, lower: Option<&[u8]>, upper: Option<&[u8]>) -> Result<DbIter> {
        self.snapshot().range(lower, upper)
    }

    /// every key that starts with `prefix`
    pub fn prefix(&self, prefix: &[u8]) -> Result<DbIter> {
        self.snapshot().prefix(prefix)
    }

    pub fn last_key_value(&self) -> Result<Option<(Vec<u8>, Vec<u8>)>> {
        let mut it = self.iter()?;
        it.seek_to_last();
        if it.valid() {
            return Ok(Some((it.key().to_vec(), it.value().to_vec())));
        }
        it.status().map(|_| None)
    }

    pub fn first_key_value(&self) -> Result<Option<(Vec<u8>, Vec<u8>)>> {
        self.iter()?.next().transpose()
    }

    pub fn begin(&self) -> Transaction<'_> {
        Transaction::new(self)
    }

    /// runs `f` in a fresh transaction and commits; on a write conflict the
    /// whole closure is re-run, up to `retries` extra times
    pub fn transact<T>(
        &self,
        retries: usize,
        mut f: impl FnMut(&mut Transaction<'_>) -> Result<T>,
    ) -> Result<T> {
        let mut left = retries;
        loop {
            let mut tx = self.begin();
            let out = f(&mut tx)?;
            match tx.commit() {
                Ok(()) => return Ok(out),
                Err(e) if e.is_conflict() && left > 0 => left -= 1,
                Err(e) => return Err(e),
            }
        }
    }

    /// sets `key` to `new` (None deletes) only if it currently holds `expected`
    /// (None = absent). false means the value differed or lost a race
    pub fn compare_and_swap(
        &self,
        key: &[u8],
        expected: Option<&[u8]>,
        new: Option<Vec<u8>>,
    ) -> Result<bool> {
        let mut tx = self.begin();
        if tx.get_for_update(key)?.as_deref() != expected {
            return Ok(false);
        }
        match new {
            Some(v) => tx.put(key, v),
            None => tx.delete(key),
        }
        match tx.commit() {
            Ok(()) => Ok(true),
            Err(e) if e.is_conflict() => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// rotates the memtable and blocks until it is on disk as an L0 table
    pub fn flush(&self) -> Result<()> {
        self.inner.flush()
    }

    /// flushes, then pushes every level down; leaves a single sorted run per level
    pub fn compact_all(&self) -> Result<()> {
        self.inner.flush()?;
        for level in 0..NUM_LEVELS - 1 {
            self.inner.compact_once(Some(level))?;
        }
        Ok(())
    }

    pub fn stats(&self) -> Stats {
        let (mem, version) = {
            let s = self.inner.state.lock().unwrap();
            (s.mem.approx_bytes(), s.version.clone())
        };
        let m = &self.inner.metrics;
        Stats {
            files_per_level: (0..NUM_LEVELS).map(|l| version.num_files(l)).collect(),
            bytes_per_level: (0..NUM_LEVELS).map(|l| version.level_bytes(l)).collect(),
            memtable_bytes: mem,
            last_seq: self.inner.last_seq.load(Ordering::Acquire),
            bloom_checked: m.bloom_checked.load(Ordering::Relaxed),
            bloom_skipped: m.bloom_skipped.load(Ordering::Relaxed),
            block_reads: m.block_reads.load(Ordering::Relaxed),
            write_ops: m.write_ops.load(Ordering::Relaxed),
            write_bytes: m.write_bytes.load(Ordering::Relaxed),
            wal_syncs: m.wal_syncs.load(Ordering::Relaxed),
            conflicts: m.conflicts.load(Ordering::Relaxed),
            stalls: m.stalls.load(Ordering::Relaxed),
            flushes: m.flushes.load(Ordering::Relaxed),
            flush_bytes: m.flush_bytes.load(Ordering::Relaxed),
            compactions: m.compactions.load(Ordering::Relaxed),
            trivial_moves: m.trivial_moves.load(Ordering::Relaxed),
            compaction_read_bytes: m.compaction_read_bytes.load(Ordering::Relaxed),
            compaction_write_bytes: m.compaction_write_bytes.load(Ordering::Relaxed),
        }
    }
}

impl Drop for Db {
    fn drop(&mut self) {
        self.inner.shutdown.store(true, Ordering::Release);
        {
            let _g = self.inner.state.lock().unwrap();
            self.inner.bg_cv.notify_all();
        }
        if let Some(h) = self.bg.lock().unwrap().take() {
            let _ = h.join();
        }
        // let go of the directory lock even if snapshots keep DbInner alive
        self.inner.lock_file.lock().unwrap().take();
    }
}

impl Snapshot {
    pub fn seq(&self) -> SeqNo {
        self.seq
    }

    pub fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>> {
        Ok(self.inner.get_at(key, self.seq)?.and_then(|h| h.value))
    }

    pub fn contains_key(&self, key: &[u8]) -> Result<bool> {
        Ok(self.get(key)?.is_some())
    }

    pub fn get_many(&self, keys: &[&[u8]]) -> Result<Vec<Option<Vec<u8>>>> {
        keys.iter().map(|k| self.get(k)).collect()
    }

    pub fn range(self, lower: Option<&[u8]>, upper: Option<&[u8]>) -> Result<DbIter> {
        DbIter::new(self, lower, upper)
    }

    pub fn prefix(self, prefix: &[u8]) -> Result<DbIter> {
        let upper = prefix_end(prefix);
        DbIter::new(self, Some(prefix), upper.as_deref())
    }
}

// smallest key greater than everything with this prefix; None if the prefix is all 0xff
pub(crate) fn prefix_end(prefix: &[u8]) -> Option<Vec<u8>> {
    let mut end = prefix.to_vec();
    while let Some(last) = end.last_mut() {
        if *last < 0xff {
            *last += 1;
            return Some(end);
        }
        end.pop();
    }
    None
}

impl Drop for Snapshot {
    fn drop(&mut self) {
        self.inner.release_snapshot(self.seq);
    }
}

impl DbInner {
    fn acquire_snapshot(self: &Arc<Self>) -> Snapshot {
        let mut l = self.snapshots.lock().unwrap();
        let seq = self.last_seq.load(Ordering::Acquire);
        *l.entry(seq).or_insert(0) += 1;
        Snapshot { inner: self.clone(), seq }
    }

    // second handle on a seq that some live snapshot already pins
    fn pin_snapshot(self: &Arc<Self>, seq: SeqNo) -> Snapshot {
        *self.snapshots.lock().unwrap().entry(seq).or_insert(0) += 1;
        Snapshot { inner: self.clone(), seq }
    }

    fn release_snapshot(&self, seq: SeqNo) {
        let mut l = self.snapshots.lock().unwrap();
        if let Some(c) = l.get_mut(&seq) {
            *c -= 1;
            if *c == 0 {
                l.remove(&seq);
            }
        }
    }

    // compaction may drop anything shadowed at or below this
    fn oldest_snapshot(&self) -> SeqNo {
        let l = self.snapshots.lock().unwrap();
        l.keys().next().copied().unwrap_or_else(|| self.last_seq.load(Ordering::Acquire))
    }

    fn get_at(&self, key: &[u8], seq: SeqNo) -> Result<Option<Hit>> {
        // state first, version via the same lock: a flush publishes the new
        // version and drops `imm` under one lock hold, so we can't miss data
        let (mem, imm, version) = {
            let s = self.state.lock().unwrap();
            (s.mem.clone(), s.imm.clone(), s.version.clone())
        };
        if let Some(h) = mem.get(key, seq) {
            return Ok(Some(h));
        }
        if let Some(h) = imm.as_ref().and_then(|m| m.get(key, seq)) {
            return Ok(Some(h));
        }
        version.get(key, seq, &self.metrics)
    }

    fn latest_seq(&self, key: &[u8]) -> Result<SeqNo> {
        Ok(self.get_at(key, MAX_SEQ)?.map_or(0, |h| h.seq))
    }

    fn set_bg_error(&self, e: Error) {
        let mut slot = self.bg_error.lock().unwrap();
        if slot.is_none() {
            *slot = Some(e);
        }
        drop(slot);
        let _g = self.state.lock().unwrap();
        self.state_cv.notify_all();
    }

    fn check_bg_error(&self) -> Result<()> {
        match &*self.bg_error.lock().unwrap() {
            Some(e) => Err(Error::Poisoned(e.to_string())),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::prefix_end;

    #[test]
    fn prefix_end_cases() {
        assert_eq!(prefix_end(b"abc"), Some(b"abd".to_vec()));
        assert_eq!(prefix_end(b"ab\xff"), Some(b"ac".to_vec()));
        assert_eq!(prefix_end(b"\xff\xff"), None);
        assert_eq!(prefix_end(b""), None);
    }
}
