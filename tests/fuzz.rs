// mutate files of a healthy db at random and make sure opening/reading never panics.
// errors (Corruption etc.) are fine, wrong-but-quiet answers for untouched data are not checked here
mod common;
use common::*;
use lsmdb::{Db, WriteBatch, WriteOptions};
use std::{
    fs,
    panic::{self, AssertUnwindSafe},
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
};

static PANICS: AtomicUsize = AtomicUsize::new(0);

fn build(dir: &Path) {
    let db = open(dir);
    let wo = WriteOptions { sync: false };
    for round in 0..4u32 {
        for i in 0..400 {
            let mut b = WriteBatch::new();
            b.put(key(i), val(i, round));
            if i % 9 == 0 {
                b.delete(key(i + 1));
            }
            db.write(b, &wo).unwrap();
        }
        if round < 3 {
            db.flush().unwrap();
        }
    }
    db.compact_all().unwrap();
    // leave a few records in the wal
    for i in 0..30 {
        db.put(key(5000 + i), val(i, 9)).unwrap();
    }
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for e in fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        if e.file_name() != "LOCK" {
            fs::copy(e.path(), to.join(e.file_name())).unwrap();
        }
    }
}

fn mutate(f: &Path, rng: &mut Rng) {
    let mut b = fs::read(f).unwrap();
    if b.is_empty() {
        return;
    }
    match rng.below(5) {
        0 => {
            for _ in 0..1 + rng.below(4) {
                let i = rng.below(b.len() as u64) as usize;
                b[i] ^= 1 << rng.below(8);
            }
        }
        1 => {
            let i = rng.below(b.len() as u64) as usize;
            b.truncate(i);
        }
        2 => {
            let i = rng.below(b.len() as u64) as usize;
            let n = (rng.below(32) as usize).min(b.len() - i);
            for x in &mut b[i..i + n] {
                *x = rng.below(256) as u8;
            }
        }
        3 => {
            for _ in 0..16 {
                b.push(rng.below(256) as u8);
            }
        }
        _ => {
            // stomp a whole aligned-ish region with 0xff / 0x00, the classic "huge length" case
            let i = rng.below(b.len() as u64) as usize;
            let fill = if rng.below(2) == 0 { 0xff } else { 0x00 };
            let n = 8.min(b.len() - i);
            b[i..i + n].fill(fill);
        }
    }
    fs::write(f, b).unwrap();
}

fn exercise(dir: &Path) {
    let Ok(db) = Db::open(dir, small_opts()) else { return };
    for i in (0..420).step_by(5) {
        let _ = db.get(&key(i));
    }
    if let Ok(it) = db.iter() {
        for kv in it {
            if kv.is_err() {
                break;
            }
        }
    }
    let _ = db.put(b"after", b"x");
    let _ = db.flush();
    let _ = db.compact_all();
}

#[test]
fn random_corruption_never_panics() {
    let base = tempfile::tempdir().unwrap();
    build(base.path());
    let files: Vec<PathBuf> = fs::read_dir(base.path())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.file_name().unwrap() != "LOCK")
        .collect();

    panic::set_hook(Box::new(|info| {
        PANICS.fetch_add(1, Ordering::SeqCst);
        eprintln!("PANIC: {info}");
    }));

    let mut rng = Rng(0x5eed_1234_abcd);
    for round in 0..150 {
        let work = tempfile::tempdir().unwrap();
        copy_dir(base.path(), work.path());
        let victim = &files[rng.below(files.len() as u64) as usize];
        mutate(&work.path().join(victim.file_name().unwrap()), &mut rng);
        if rng.below(4) == 0 {
            let other = &files[rng.below(files.len() as u64) as usize];
            mutate(&work.path().join(other.file_name().unwrap()), &mut rng);
        }
        let r = panic::catch_unwind(AssertUnwindSafe(|| exercise(work.path())));
        assert!(r.is_ok() && PANICS.load(Ordering::SeqCst) == 0, "panic on round {round}, file {victim:?}");
    }
}
