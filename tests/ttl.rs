mod common;
use common::*;
use lsmdb::{CompactionFilter, Options, TtlDb};
use std::{
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};

fn open_at(dir: &std::path::Path, now: &Arc<AtomicU64>, opts: Options) -> TtlDb {
    let n = now.clone();
    TtlDb::open_with_clock(dir, opts, Arc::new(move || n.load(Ordering::SeqCst))).unwrap()
}

#[test]
fn entries_expire() {
    let d = tempfile::tempdir().unwrap();
    let now = Arc::new(AtomicU64::new(1_000));
    let db = open_at(d.path(), &now, small_opts());
    db.put("short", b"a", Duration::from_secs(1)).unwrap();
    db.put("long", b"b", Duration::from_secs(60)).unwrap();
    db.put_forever("forever", b"c").unwrap();

    assert_eq!(db.get(b"short").unwrap().unwrap(), b"a");
    now.store(1_000 + 999, Ordering::SeqCst);
    assert!(db.get(b"short").unwrap().is_some());
    now.store(1_000 + 1_000, Ordering::SeqCst);
    assert!(db.get(b"short").unwrap().is_none());
    assert_eq!(db.get(b"long").unwrap().unwrap(), b"b");

    now.store(u64::MAX - 5, Ordering::SeqCst);
    assert!(db.get(b"long").unwrap().is_none());
    assert_eq!(db.get(b"forever").unwrap().unwrap(), b"c");
    assert!(db.get(b"missing").unwrap().is_none());
}

#[test]
fn iteration_skips_expired_and_overwrite_renews() {
    let d = tempfile::tempdir().unwrap();
    let now = Arc::new(AtomicU64::new(0));
    let db = open_at(d.path(), &now, small_opts());
    for i in 0..10 {
        db.put(key(i), b"v", Duration::from_millis(100 + i as u64)).unwrap();
    }
    now.store(105, Ordering::SeqCst);
    let live: Vec<_> = db.iter().unwrap().map(|r| r.unwrap().0).collect();
    assert_eq!(live.len(), 4);
    assert_eq!(live[0], key(6));

    db.put(key(0), b"again", Duration::from_secs(10)).unwrap();
    assert_eq!(db.get(&key(0)).unwrap().unwrap(), b"again");
    assert_eq!(db.iter().unwrap().count(), 5);
}

#[test]
fn compaction_reclaims_expired_entries() {
    let d = tempfile::tempdir().unwrap();
    let now = Arc::new(AtomicU64::new(0));
    let db = open_at(d.path(), &now, small_opts());
    for i in 0..400 {
        db.put(key(i), &val(i, 0), Duration::from_millis(if i % 2 == 0 { 10 } else { 100_000 })).unwrap();
    }
    db.db().flush().unwrap();
    now.store(50, Ordering::SeqCst);
    db.db().compact_all().unwrap();

    assert_eq!(db.db().stats().filtered, 200);
    assert_eq!(db.iter().unwrap().count(), 200);
    // raw view really lost them, not just hidden
    assert_eq!(db.db().iter().unwrap().count(), 200);
}

#[test]
fn user_filter_still_runs() {
    let d = tempfile::tempdir().unwrap();
    let now = Arc::new(AtomicU64::new(0));
    let opts =
        Options { compaction_filter: Some(CompactionFilter::new(|_, v| v == b"junk")), ..small_opts() };
    let db = open_at(d.path(), &now, opts);
    db.put_forever("a", b"junk").unwrap();
    db.put_forever("b", b"fine").unwrap();
    db.db().flush().unwrap();
    db.db().compact_all().unwrap();
    assert!(db.get(b"a").unwrap().is_none());
    assert_eq!(db.get(b"b").unwrap().unwrap(), b"fine");
}

#[test]
fn survives_reopen() {
    let d = tempfile::tempdir().unwrap();
    let now = Arc::new(AtomicU64::new(5));
    {
        let db = open_at(d.path(), &now, small_opts());
        db.put("k", b"v", Duration::from_secs(5)).unwrap();
    }
    let db = open_at(d.path(), &now, small_opts());
    assert_eq!(db.get(b"k").unwrap().unwrap(), b"v");
    now.store(6_000, Ordering::SeqCst);
    assert!(db.get(b"k").unwrap().is_none());
}
