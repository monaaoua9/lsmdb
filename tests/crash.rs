// kill -9 style crash test: the test binary re-executes itself as a writer,
// the parent kills it at a random moment and checks what survived.
mod common;
use common::*;
use lsmdb::{Db, WriteBatch, WriteOptions};
use std::{
    env,
    io::{BufRead, BufReader, Write},
    path::Path,
    process::{Command, Stdio},
    thread,
    time::Duration,
};

const CHILD_DIR: &str = "LSMDB_CRASH_CHILD_DIR";

// not a real test: it is the child body, and only does anything when spawned by `crash_recovery`
#[test]
fn child_entry() {
    let Ok(dir) = env::var(CHILD_DIR) else { return };
    let db = Db::open(&dir, small_opts()).unwrap();
    let start: u32 = env::var("LSMDB_CRASH_START").unwrap().parse().unwrap();
    let sync = env::var("LSMDB_CRASH_SYNC").unwrap() == "1";
    let wo = WriteOptions { sync };
    let out = std::io::stdout();
    for i in start..=u32::MAX {
        // each batch writes two keys; atomicity means both or neither show up
        let mut b = WriteBatch::new();
        b.put(key(i), val(i, 0));
        b.put(format!("mirror-{i:08}").into_bytes(), val(i, 0));
        db.write(b, &wo).unwrap();
        // only printed after the write call returned, i.e. acknowledged
        let mut h = out.lock();
        writeln!(h, "{i}").unwrap();
        h.flush().unwrap();
    }
}

fn run_child(dir: &Path, start: u32, sync: bool, ms: u64) -> Option<u32> {
    let mut child = Command::new(env::current_exe().unwrap())
        .args(["--exact", "child_entry", "--nocapture", "--test-threads=1"])
        .env(CHILD_DIR, dir)
        .env("LSMDB_CRASH_START", start.to_string())
        .env("LSMDB_CRASH_SYNC", if sync { "1" } else { "0" })
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let out = child.stdout.take().unwrap();
    let reader = thread::spawn(move || {
        let mut last = None;
        for line in BufReader::new(out).lines().map_while(|l| l.ok()) {
            if let Ok(n) = line.trim().parse::<u32>() {
                last = Some(n);
            }
        }
        last
    });
    thread::sleep(Duration::from_millis(ms));
    child.kill().unwrap();
    child.wait().unwrap();
    reader.join().unwrap()
}

fn check(dir: &Path, acked_upto: Option<u32>, start: u32) -> u32 {
    let db = open(dir);
    // recovered data must be a prefix: find the first missing key, nothing may exist beyond it
    let mut next = 0;
    while db.get(&key(next)).unwrap().is_some() {
        assert_eq!(db.get(&key(next)).unwrap().unwrap(), val(next, 0));
        // batch atomicity
        assert_eq!(db.get(format!("mirror-{next:08}").as_bytes()).unwrap().unwrap(), val(next, 0));
        next += 1;
    }
    for j in next..next + 50 {
        assert!(db.get(&key(j)).unwrap().is_none(), "hole before {j}");
        assert!(db.get(format!("mirror-{j:08}").as_bytes()).unwrap().is_none());
    }
    if let Some(a) = acked_upto {
        assert!(next > a, "acknowledged write {a} lost, recovered only {next} keys");
    }
    assert!(next >= start);
    next
}

fn rounds(sync: bool, n: u32) {
    let d = tempfile::tempdir().unwrap();
    let mut start = 0;
    let mut rng = Rng(0xfeed_f00d + n as u64);
    for _ in 0..n {
        let acked = run_child(d.path(), start, sync, 150 + rng.below(700));
        // unsynced writes may or may not survive a process kill, but they must still form a prefix
        start = check(d.path(), if sync { acked } else { None }, start);
    }
    assert!(start > 0);
}

#[test]
fn crash_recovery_sync() {
    rounds(true, 12);
}

#[test]
fn crash_recovery_nosync() {
    // process kill (not power loss): the OS still has the written pages
    rounds(false, 8);
}
