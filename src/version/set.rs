use std::{
    collections::HashMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering::SeqCst},
        Arc,
    },
};

use super::{FileMeta, NewFile, Version, VersionEdit, NUM_LEVELS};
use crate::{
    error::{Error, Result},
    filename::{current_path, manifest_name, table_path},
    options::Options,
    types::cmp_ikey,
    wal::{LogReader, LogWriter},
};

/// Owns the manifest and the current Version. Only touched with the
/// DbInner::versions mutex held.
pub struct VersionSet {
    dir: PathBuf,
    verify: bool,
    next_file: Arc<AtomicU64>,
    log_number: u64,
    last_seq: u64,
    current: Arc<Version>,
    manifest: LogWriter,
    manifest_number: u64,
    compact_ptr: Vec<Vec<u8>>,
}

fn sort_level(level: usize, files: &mut [Arc<FileMeta>]) {
    if level == 0 {
        files.sort_by_key(|f| std::cmp::Reverse(f.number));
    } else {
        files.sort_by(|a, b| cmp_ikey(&a.smallest, &b.smallest));
    }
}

impl VersionSet {
    pub fn open(dir: &Path, opts: &Options, next_file: Arc<AtomicU64>) -> Result<VersionSet> {
        let cur = current_path(dir);
        let mut raw: Vec<Vec<NewFile>> = vec![Vec::new(); NUM_LEVELS];
        let (mut log_number, mut last_seq, mut next) = (0, 0, 2);

        if cur.exists() {
            if opts.error_if_exists {
                return Err(Error::invalid("database already exists"));
            }
            let name = fs::read_to_string(&cur)?;
            let mut r = LogReader::open(&dir.join(name.trim()))?;
            while let Some(rec) = r.read_record() {
                let e = VersionEdit::decode(rec)?;
                for (level, num) in &e.deleted {
                    if *level >= NUM_LEVELS {
                        return Err(Error::corruption("bad level in manifest"));
                    }
                    raw[*level].retain(|f| f.number != *num);
                }
                for (level, f) in e.added {
                    if level >= NUM_LEVELS {
                        return Err(Error::corruption("bad level in manifest"));
                    }
                    raw[level].push(f);
                }
                log_number = e.log_number.unwrap_or(log_number);
                last_seq = e.last_seq.unwrap_or(last_seq);
                next = e.next_file.unwrap_or(next).max(next);
            }
        } else if !opts.create_if_missing {
            return Err(Error::invalid("database does not exist"));
        }

        let mut levels = Vec::with_capacity(NUM_LEVELS);
        for (i, files) in raw.into_iter().enumerate() {
            let mut v: Vec<_> = files
                .into_iter()
                .map(|f| Arc::new(FileMeta::new(table_path(dir, f.number), opts.verify_checksums, f)))
                .collect();
            sort_level(i, &mut v);
            levels.push(v);
        }
        next_file.fetch_max(next, SeqCst);

        let manifest_number = next_file.fetch_add(1, SeqCst);
        let manifest = Self::write_snapshot(dir, manifest_number, &levels, log_number, last_seq, &next_file)?;
        Ok(VersionSet {
            dir: dir.to_path_buf(),
            verify: opts.verify_checksums,
            next_file,
            log_number,
            last_seq,
            current: Arc::new(Version { levels }),
            manifest,
            manifest_number,
            compact_ptr: vec![Vec::new(); NUM_LEVELS],
        })
    }

    // fresh manifest holding the full state, then CURRENT flips to it via rename
    fn write_snapshot(
        dir: &Path,
        number: u64,
        levels: &[Vec<Arc<FileMeta>>],
        log_number: u64,
        last_seq: u64,
        next_file: &AtomicU64,
    ) -> Result<LogWriter> {
        let name = manifest_name(number);
        let mut w = LogWriter::create(&dir.join(&name))?;
        let mut e = VersionEdit {
            log_number: Some(log_number),
            next_file: Some(next_file.load(SeqCst)),
            last_seq: Some(last_seq),
            ..Default::default()
        };
        for (level, files) in levels.iter().enumerate() {
            for f in files {
                e.added.push((
                    level,
                    NewFile {
                        number: f.number,
                        size: f.size,
                        smallest: f.smallest.clone(),
                        largest: f.largest.clone(),
                    },
                ));
            }
        }
        w.append(&e.encode())?;
        w.sync()?;

        let tmp = dir.join("CURRENT.tmp");
        // FlushFileBuffers on windows needs a writable handle, so no write + reopen-readonly
        let mut f = fs::File::create(&tmp)?;
        f.write_all(format!("{name}\n").as_bytes())?;
        f.sync_all()?;
        drop(f);
        fs::rename(&tmp, current_path(dir))?;
        Ok(w)
    }

    pub fn current(&self) -> Arc<Version> {
        self.current.clone()
    }

    pub fn log_number(&self) -> u64 {
        self.log_number
    }

    pub fn last_seq(&self) -> u64 {
        self.last_seq
    }

    pub fn manifest_number(&self) -> u64 {
        self.manifest_number
    }

    pub fn compact_ptr(&self) -> &[Vec<u8>] {
        &self.compact_ptr
    }

    pub fn set_compact_ptr(&mut self, level: usize, key: Vec<u8>) {
        self.compact_ptr[level] = key;
    }

    pub fn live_files(&self) -> Vec<u64> {
        self.current.levels.iter().flatten().map(|f| f.number).collect()
    }

    /// applies the edit to the current version, persists it, and returns the new version
    pub fn log_and_apply(&mut self, mut edit: VersionEdit, last_seq: u64) -> Result<Arc<Version>> {
        edit.next_file = Some(self.next_file.load(SeqCst));
        edit.last_seq = Some(last_seq);
        if edit.log_number.is_none() {
            edit.log_number = Some(self.log_number);
        }

        let old = &self.current;
        let existing: HashMap<u64, Arc<FileMeta>> =
            old.levels.iter().flatten().map(|f| (f.number, f.clone())).collect();
        let readded = |n: u64| edit.added.iter().any(|(_, f)| f.number == n);

        let mut levels = old.levels.clone();
        for (level, num) in &edit.deleted {
            levels[*level].retain(|f| f.number != *num);
        }
        for (level, f) in &edit.added {
            // trivial moves re-add a file under a new level; keep the same FileMeta
            let meta = existing.get(&f.number).cloned().unwrap_or_else(|| {
                Arc::new(FileMeta::new(table_path(&self.dir, f.number), self.verify, f.clone()))
            });
            levels[*level].push(meta);
        }
        for (i, files) in levels.iter_mut().enumerate() {
            sort_level(i, files);
        }

        self.manifest.append(&edit.encode())?;
        self.manifest.sync()?;

        for (_, num) in &edit.deleted {
            if !readded(*num) {
                if let Some(f) = existing.get(num) {
                    f.mark_obsolete();
                }
            }
        }
        self.log_number = edit.log_number.unwrap();
        self.last_seq = last_seq;
        self.current = Arc::new(Version { levels });
        Ok(self.current.clone())
    }
}
