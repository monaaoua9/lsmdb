use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use super::{DbInner, State};
use crate::{
    batch::{encode_op, WriteBatch, RECORD_HEADER},
    coding::{put_fixed32, put_fixed64},
    error::{Error, Result},
    filename::log_path,
    memtable::MemTable,
    types::{SeqNo, MAX_SEQ},
    wal::LogWriter,
};

pub(super) struct ConflictCheck {
    pub start_seq: SeqNo,
    pub keys: Vec<Vec<u8>>,
}

pub(super) struct Writer {
    batch: WriteBatch,
    sync: bool,
    force_flush: bool,
    conflict: Option<ConflictCheck>,
    done: AtomicBool,
    result: Mutex<Option<Result<()>>>,
}

impl Writer {
    pub fn new(batch: WriteBatch, sync: bool) -> Self {
        Writer {
            batch,
            sync,
            force_flush: false,
            conflict: None,
            done: AtomicBool::new(false),
            result: Mutex::new(None),
        }
    }

    pub fn with_conflict(batch: WriteBatch, sync: bool, c: ConflictCheck) -> Self {
        Writer { conflict: Some(c), ..Self::new(batch, sync) }
    }

    fn flush_marker() -> Self {
        Writer { force_flush: true, ..Self::new(WriteBatch::new(), false) }
    }

    fn take_result(&self) -> Result<()> {
        self.result.lock().unwrap().clone().unwrap_or(Ok(()))
    }
}

// cap on how much a leader folds into one WAL record
const MAX_GROUP_BYTES: usize = 1 << 20;

impl DbInner {
    // Leader/follower group commit: the writer at the head of the queue
    // becomes leader, takes everyone queued behind it, does one WAL append
    // and one fsync for the lot, then hands out results.
    pub(super) fn submit(&self, w: Writer) -> Result<()> {
        if self.read_only {
            return Err(Error::ReadOnly);
        }
        self.check_bg_error()?;
        let w = Arc::new(w);
        let mut q = self.queue.lock().unwrap();
        q.push_back(w.clone());
        while !w.done.load(Ordering::Acquire) && !Arc::ptr_eq(q.front().unwrap(), &w) {
            q = self.queue_cv.wait(q).unwrap();
        }
        if w.done.load(Ordering::Acquire) {
            return w.take_result();
        }

        let mut group = vec![w.clone()];
        let mut bytes = w.batch.byte_size();
        for next in q.iter().skip(1) {
            bytes += next.batch.byte_size();
            if bytes > MAX_GROUP_BYTES {
                break;
            }
            group.push(next.clone());
        }
        drop(q);

        let results = match self.write_group(&group) {
            Ok(r) => r,
            Err(e) => vec![Err(e); group.len()],
        };

        let mut q = self.queue.lock().unwrap();
        for (g, r) in group.iter().zip(results) {
            q.pop_front();
            *g.result.lock().unwrap() = Some(r);
            g.done.store(true, Ordering::Release);
        }
        drop(q);
        self.queue_cv.notify_all();
        w.take_result()
    }

    fn write_group(&self, group: &[Arc<Writer>]) -> Result<Vec<Result<()>>> {
        let force = group.iter().any(|w| w.force_flush);
        let mem = self.make_room_for_write(force)?;

        // only the leader touches last_seq, so relaxed is enough here
        let first_seq = self.last_seq.load(Ordering::Relaxed) + 1;
        let mut seq = first_seq;
        let mut results: Vec<Result<()>> = vec![Ok(()); group.len()];
        let mut applied: Vec<(SeqNo, usize, usize)> = Vec::new(); // (seq, writer, op)
        let mut group_seqs: HashMap<&[u8], SeqNo> = HashMap::new();

        let mut rec = Vec::with_capacity(RECORD_HEADER + 256);
        put_fixed64(&mut rec, first_seq);
        put_fixed32(&mut rec, 0);

        'writers: for (i, w) in group.iter().enumerate() {
            if let Some(c) = &w.conflict {
                for k in &c.keys {
                    // earlier writers in this group aren't in the memtable yet
                    let latest = match group_seqs.get(k.as_slice()) {
                        Some(s) => *s,
                        None => self.latest_seq(k)?,
                    };
                    if latest > c.start_seq {
                        self.metrics.conflicts.fetch_add(1, Ordering::Relaxed);
                        results[i] = Err(Error::Conflict);
                        continue 'writers;
                    }
                }
            }
            for (j, op) in w.batch.ops.iter().enumerate() {
                encode_op(&mut rec, op.kind, &op.key, &op.value);
                group_seqs.insert(&op.key, seq);
                applied.push((seq, i, j));
                seq += 1;
            }
        }

        if applied.is_empty() {
            return Ok(results);
        }
        let count = (seq - first_seq) as u32;
        rec[8..12].copy_from_slice(&count.to_le_bytes());

        let sync = group.iter().any(|w| w.sync);
        let m = &self.metrics;
        m.write_ops.fetch_add(applied.len() as u64, Ordering::Relaxed);
        let payload: usize =
            results.iter().zip(group).filter(|(r, _)| r.is_ok()).map(|(_, w)| w.batch.byte_size()).sum();
        m.write_bytes.fetch_add(payload as u64, Ordering::Relaxed);
        if sync {
            m.wal_syncs.fetch_add(1, Ordering::Relaxed);
        }
        let wal_res = {
            let mut guard = self.wal.lock().unwrap();
            let wal = guard.as_mut().expect("writable db has a wal");
            wal.append(&rec).and_then(|_| if sync { wal.sync() } else { Ok(()) })
        };
        if let Err(e) = wal_res {
            // can't know how much of the record hit the disk; stop accepting writes
            self.set_bg_error(e.clone());
            return Err(e);
        }

        for (s, wi, oi) in applied {
            let op = &group[wi].batch.ops[oi];
            mem.add(s, op.kind, &op.key, &op.value);
        }
        // publish only after every entry is in the memtable: a batch is
        // all-or-nothing to readers
        debug_assert!(seq - 1 <= MAX_SEQ);
        self.last_seq.store(seq - 1, Ordering::Release);
        Ok(results)
    }

    fn make_room_for_write(&self, force: bool) -> Result<Arc<MemTable>> {
        let mut slowed = false;
        let mut s = self.state.lock().unwrap();
        loop {
            self.check_bg_error()?;
            let l0 = s.version.num_files(0);
            if l0 >= self.opts.l0_stop_trigger {
                self.metrics.stalls.fetch_add(1, Ordering::Relaxed);
                s = self.state_cv.wait_timeout(s, Duration::from_millis(50)).unwrap().0;
            } else if l0 >= self.opts.l0_slowdown_trigger && !slowed {
                // soften the cliff: give compaction a little breathing room, once per write
                drop(s);
                self.metrics.stalls.fetch_add(1, Ordering::Relaxed);
                std::thread::sleep(Duration::from_millis(1));
                slowed = true;
                s = self.state.lock().unwrap();
            } else if (!force && s.mem.approx_bytes() < self.opts.write_buffer_size)
                || (force && s.mem.is_empty())
            {
                return Ok(s.mem.clone());
            } else if s.imm.is_some() {
                s = self.state_cv.wait_timeout(s, Duration::from_millis(50)).unwrap().0;
            } else {
                self.rotate_memtable(&mut s)?;
                self.bg_cv.notify_all();
                return Ok(s.mem.clone());
            }
        }
    }

    // caller holds the state lock and is the current group leader
    fn rotate_memtable(&self, s: &mut State) -> Result<()> {
        let number = self.next_file.fetch_add(1, Ordering::SeqCst);
        let new_wal = LogWriter::create(&log_path(&self.dir, number))?;
        {
            let mut wal = self.wal.lock().unwrap();
            // anything written with sync=false must be durable before its log is retired
            if let Some(old) = wal.as_mut() {
                old.sync()?;
            }
            *wal = Some(new_wal);
        }
        s.imm = Some(std::mem::replace(&mut s.mem, Arc::new(MemTable::new())));
        s.imm_next_log = number;
        Ok(())
    }

    pub(super) fn flush(&self) -> Result<()> {
        self.submit(Writer::flush_marker())?;
        let mut s = self.state.lock().unwrap();
        while s.imm.is_some() {
            self.check_bg_error()?;
            s = self.state_cv.wait_timeout(s, Duration::from_millis(50)).unwrap().0;
        }
        Ok(())
    }
}
