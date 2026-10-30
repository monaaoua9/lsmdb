use std::{
    fs,
    path::Path,
    sync::atomic::Ordering::{self, Relaxed},
    time::Duration,
};

use super::DbInner;
use crate::{
    compaction::{self, Compaction},
    error::{Error, Result},
    filename::{self, table_path, FileKind},
    iter::{BoxIter, InternalIterator, MergingIterator},
    options::Options,
    sstable::{TableBuilder, TableInfo},
    types::{make_ikey, parse_ikey, user_key, ValueKind, MAX_SEQ},
    version::{NewFile, VersionEdit, VersionSet},
};

/// drains `it` into a new table file; removes the half-written file on failure
pub(super) fn write_table(
    dir: &Path,
    opts: &Options,
    number: u64,
    it: &mut dyn InternalIterator,
) -> Result<TableInfo> {
    let mut b = TableBuilder::new(&table_path(dir, number), opts)?;
    let res = (|| {
        while it.valid() {
            b.add(it.key(), it.value())?;
            it.next();
        }
        it.status()
    })();
    match res {
        Ok(()) => b.finish(),
        Err(e) => {
            b.abandon();
            Err(e)
        }
    }
}

// called on open: drop whatever the manifest doesn't know about
pub(super) fn sweep_obsolete(dir: &Path, vs: &VersionSet) {
    let live = vs.live_files();
    let Ok(rd) = fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let stale = match filename::parse(&name) {
            FileKind::Log(n) => n < vs.log_number(),
            FileKind::Table(n) => !live.contains(&n),
            FileKind::Manifest(n) => n != vs.manifest_number(),
            FileKind::Temp => true,
            FileKind::Other => false,
        };
        if stale {
            let _ = fs::remove_file(e.path());
        }
    }
}

impl DbInner {
    pub(super) fn bg_loop(&self) {
        loop {
            {
                let mut s = self.state.lock().unwrap();
                loop {
                    if self.shutdown.load(Ordering::Acquire) {
                        return;
                    }
                    if s.imm.is_some() || compaction::needs_compaction(&s.version, &self.opts) {
                        break;
                    }
                    s = self.bg_cv.wait_timeout(s, Duration::from_millis(500)).unwrap().0;
                }
            }
            match self.bg_work() {
                Ok(true) => {}
                // woken but nothing to do (raced with a manual compaction)
                Ok(false) => std::thread::sleep(Duration::from_millis(10)),
                Err(e) => {
                    self.set_bg_error(e);
                    return;
                }
            }
        }
    }

    fn bg_work(&self) -> Result<bool> {
        let has_imm = self.state.lock().unwrap().imm.is_some();
        if has_imm {
            self.flush_imm()?;
            return Ok(true);
        }
        self.compact_once(None)
    }

    fn flush_imm(&self) -> Result<()> {
        let (imm, next_log) = {
            let s = self.state.lock().unwrap();
            match &s.imm {
                Some(m) => (m.clone(), s.imm_next_log),
                None => return Ok(()),
            }
        };
        let mut edit = VersionEdit { log_number: Some(next_log), ..Default::default() };
        if !imm.is_empty() {
            let number = self.next_file.fetch_add(1, Ordering::SeqCst);
            let mut it = imm.iter();
            it.seek_to_first();
            let info = write_table(&self.dir, &self.opts, number, &mut it)?;
            self.metrics.flush_bytes.fetch_add(info.size, Relaxed);
            edit.added.push((
                0,
                NewFile { number, size: info.size, smallest: info.smallest, largest: info.largest },
            ));
        }
        self.install(edit, true)?;
        self.metrics.flushes.fetch_add(1, Relaxed);

        // logs older than the one the new memtable started with are covered by the table now
        if let Ok(rd) = fs::read_dir(&self.dir) {
            for e in rd.flatten() {
                if let FileKind::Log(n) = filename::parse(&e.file_name().to_string_lossy()) {
                    if n < next_log {
                        let _ = fs::remove_file(e.path());
                    }
                }
            }
        }
        Ok(())
    }

    // publishes under the versions lock so two installers can never swap in
    // each other's stale version
    fn install(&self, edit: VersionEdit, clear_imm: bool) -> Result<()> {
        let mut vs = self.versions.lock().unwrap();
        let v = vs.log_and_apply(edit, self.last_seq.load(Ordering::Acquire))?;
        let old = {
            let mut s = self.state.lock().unwrap();
            if clear_imm {
                s.imm = None;
            }
            std::mem::replace(&mut s.version, v)
        };
        drop(vs);
        // may unlink obsolete files, keep it outside the locks
        drop(old);
        self.state_cv.notify_all();
        Ok(())
    }

    /// picks and runs one compaction; Ok(false) if there was nothing to do
    pub(super) fn compact_once(&self, force: Option<usize>) -> Result<bool> {
        let _g = self.compaction_lock.lock().unwrap();
        let c = {
            let vs = self.versions.lock().unwrap();
            compaction::pick(&vs.current(), vs.compact_ptr(), &self.opts, force)
        };
        match c {
            Some(c) => {
                self.run_compaction(c)?;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    fn run_compaction(&self, c: Compaction) -> Result<()> {
        let out_level = c.level + 1;
        let mut edit = VersionEdit::default();

        // with a filter configured every table has to be read at least once on its way down
        if c.is_trivial_move() && self.opts.compaction_filter.is_none() {
            let f = &c.inputs[0][0];
            edit.deleted.push((c.level, f.number));
            edit.added.push((
                out_level,
                NewFile {
                    number: f.number,
                    size: f.size,
                    smallest: f.smallest.clone(),
                    largest: f.largest.clone(),
                },
            ));
            self.versions.lock().unwrap().set_compact_ptr(c.level, f.largest.clone());
            self.metrics.trivial_moves.fetch_add(1, Relaxed);
            return self.install(edit, false);
        }

        let mut children: Vec<BoxIter> = Vec::new();
        for f in c.inputs.iter().flatten() {
            children.push(Box::new(f.table()?.iter()));
            edit.deleted
                .push((if children.len() <= c.inputs[0].len() { c.level } else { out_level }, f.number));
        }
        let mut merge = MergingIterator::new(children);
        merge.seek_to_first();

        let smallest_snapshot = self.oldest_snapshot();
        // a snapshot could still need the value the filter wants gone
        let filter =
            self.opts.compaction_filter.as_ref().filter(|_| self.snapshots.lock().unwrap().is_empty());
        let mut outputs: Vec<NewFile> = Vec::new();
        let mut cur: Option<(u64, TableBuilder)> = None;
        let mut cur_user: Vec<u8> = Vec::new();
        let mut have_user = false;
        let mut last_seq_for_key = MAX_SEQ + 1;

        let res: Result<()> = (|| {
            while merge.valid() {
                let k = merge.key();
                let (uk, seq, kind) =
                    parse_ikey(k).ok_or_else(|| Error::corruption("bad key during compaction"))?;

                if !have_user || uk != cur_user.as_slice() {
                    cur_user.clear();
                    cur_user.extend_from_slice(uk);
                    have_user = true;
                    last_seq_for_key = MAX_SEQ + 1;
                }

                // an entry is invisible to everyone if a newer entry for the same key is
                // already at or below the oldest snapshot. tombstones can also go, but only
                // when nothing older for the key could be sitting in a deeper level.
                let mut drop_it = if last_seq_for_key <= smallest_snapshot {
                    true
                } else {
                    kind == ValueKind::Delete && seq <= smallest_snapshot && c.is_base_level_for_key(uk)
                };
                last_seq_for_key = seq;

                // a filtered put turns into a tombstone unless nothing deeper holds an older value
                let mut tomb = None;
                if let Some(f) = filter {
                    if !drop_it && kind == ValueKind::Put && f.should_remove(uk, merge.value()) {
                        self.metrics.filtered.fetch_add(1, Relaxed);
                        if c.is_base_level_for_key(uk) {
                            drop_it = true;
                        } else {
                            tomb = Some(make_ikey(uk, seq, ValueKind::Delete));
                        }
                    }
                }

                if !drop_it {
                    // split only between user keys; versions of one key stay in one file
                    let split = cur.as_ref().is_some_and(|(_, b)| {
                        b.file_size() >= self.opts.target_file_size && user_key(b.last_key()) != uk
                    });
                    if split {
                        let (n, b) = cur.take().unwrap();
                        outputs.push(finish_output(n, b)?);
                    }
                    if cur.is_none() {
                        let n = self.next_file.fetch_add(1, Ordering::SeqCst);
                        cur = Some((n, TableBuilder::new(&table_path(&self.dir, n), &self.opts)?));
                    }
                    let b = &mut cur.as_mut().unwrap().1;
                    match &tomb {
                        Some(t) => b.add(t, &[])?,
                        None => b.add(k, merge.value())?,
                    }
                }
                merge.next();
            }
            merge.status()?;
            if let Some((n, b)) = cur.take() {
                outputs.push(finish_output(n, b)?);
            }
            Ok(())
        })();

        if let Err(e) = res {
            if let Some((_, b)) = cur.take() {
                b.abandon();
            }
            for o in &outputs {
                let _ = fs::remove_file(table_path(&self.dir, o.number));
            }
            return Err(e);
        }

        if let Some(last) = c.inputs[0].iter().map(|f| &f.largest).max_by(|a, b| crate::types::cmp_ikey(a, b))
        {
            self.versions.lock().unwrap().set_compact_ptr(c.level, last.clone());
        }
        let m = &self.metrics;
        m.compactions.fetch_add(1, Relaxed);
        m.compaction_read_bytes.fetch_add(c.inputs.iter().flatten().map(|f| f.size).sum(), Relaxed);
        m.compaction_write_bytes.fetch_add(outputs.iter().map(|o| o.size).sum(), Relaxed);
        edit.added.extend(outputs.into_iter().map(|o| (out_level, o)));
        self.install(edit, false)
    }
}

fn finish_output(number: u64, b: TableBuilder) -> Result<NewFile> {
    let info = b.finish()?;
    Ok(NewFile { number, size: info.size, smallest: info.smallest, largest: info.largest })
}
