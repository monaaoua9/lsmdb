mod common;
use common::*;
use lsmdb::{Options, WriteBatch, WriteOptions};
use std::{collections::BTreeMap, sync::Arc, thread};

// randomized ops checked against a BTreeMap, with flushes, compactions and
// reopens mixed in
#[test]
fn model_check_with_reopens() {
    let d = tempfile::tempdir().unwrap();
    let mut rng = Rng(0xdead_beef_cafe_f00d);
    let mut model: BTreeMap<Vec<u8>, Vec<u8>> = BTreeMap::new();
    let mut db = Some(open(d.path()));

    for step in 0..12_000u32 {
        let h = db.as_ref().unwrap();
        let k = key(rng.below(700) as u32);
        match rng.below(100) {
            0..=54 => {
                let v = val(step, rng.below(1000) as u32);
                h.put(k.clone(), v.clone()).unwrap();
                model.insert(k, v);
            }
            55..=69 => {
                h.delete(k.clone()).unwrap();
                model.remove(&k);
            }
            70..=74 => {
                let mut b = WriteBatch::new();
                for _ in 0..5 {
                    let k = key(rng.below(700) as u32);
                    if rng.below(3) == 0 {
                        b.delete(k.clone());
                        model.remove(&k);
                    } else {
                        let v = val(step, 7);
                        b.put(k.clone(), v.clone());
                        model.insert(k, v);
                    }
                }
                h.write(b, &WriteOptions { sync: false }).unwrap();
            }
            75..=94 => assert_eq!(h.get(&k).unwrap().as_ref(), model.get(&k), "step {step}"),
            95..=96 => h.flush().unwrap(),
            97 => h.compact_all().unwrap(),
            98 => {
                let lo = key(rng.below(700) as u32);
                let hi = key(rng.below(700) as u32);
                let got: Vec<_> = h.range(Some(&lo), Some(&hi)).unwrap().map(|r| r.unwrap()).collect();
                let want: Vec<_> = if lo < hi {
                    model.range(lo..hi).map(|(k, v)| (k.clone(), v.clone())).collect()
                } else {
                    vec![]
                };
                assert_eq!(got, want, "range at step {step}");
            }
            _ => {
                // dropping the handle is the close; lock must be free for the reopen
                drop(db.take());
                db = Some(open(d.path()));
            }
        }
    }

    let h = db.unwrap();
    let got: Vec<_> = h.iter().unwrap().map(|r| r.unwrap()).collect();
    let want: Vec<_> = model.into_iter().collect();
    assert_eq!(got, want);
}

#[test]
fn concurrent_writers_group_commit() {
    let d = tempfile::tempdir().unwrap();
    let db = Arc::new(open(d.path()));
    let threads = 8u32;
    let per = 400u32;
    let hs: Vec<_> = (0..threads)
        .map(|t| {
            let db = db.clone();
            thread::spawn(move || {
                for i in 0..per {
                    db.put(key(t * per + i), val(t, i)).unwrap();
                }
            })
        })
        .collect();
    for h in hs {
        h.join().unwrap();
    }
    for t in 0..threads {
        for i in 0..per {
            assert_eq!(db.get(&key(t * per + i)).unwrap().unwrap(), val(t, i));
        }
    }
    assert_eq!(db.iter().unwrap().count() as u32, threads * per);
}

#[test]
fn readers_writers_and_background_work_together() {
    let d = tempfile::tempdir().unwrap();
    let db = Arc::new(open(d.path()));
    let n = 300u32;
    for i in 0..n {
        db.put(key(i), val(i, 0)).unwrap();
    }

    let writers: Vec<_> = (0..3)
        .map(|w| {
            let db = db.clone();
            thread::spawn(move || {
                let mut rng = Rng(0x9e37 + w as u64);
                for gen in 1..=1500u32 {
                    let i = rng.below(n as u64) as u32;
                    // generation only ever goes up per key via embedding writer id + gen
                    db.put(key(i), val(i, gen)).unwrap();
                }
            })
        })
        .collect();
    let readers: Vec<_> = (0..3)
        .map(|r| {
            let db = db.clone();
            thread::spawn(move || {
                let mut rng = Rng(0x51ed + r as u64);
                for _ in 0..3000 {
                    let i = rng.below(n as u64) as u32;
                    let v = db.get(&key(i)).unwrap().expect("key must never vanish");
                    assert!(v.starts_with(format!("value-{i}-gen").as_bytes()));
                }
                let full = db.iter().unwrap().count();
                assert_eq!(full as u32, n);
            })
        })
        .collect();
    for h in writers.into_iter().chain(readers) {
        h.join().unwrap();
    }
    db.flush().unwrap();
    assert_eq!(db.iter().unwrap().count() as u32, n);
}

#[test]
fn compaction_actually_reclaims_space() {
    let d = tempfile::tempdir().unwrap();
    let db = open(d.path());
    for gen in 0..6 {
        for i in 0..1500 {
            db.put(key(i), val(i, gen)).unwrap();
        }
        db.flush().unwrap();
    }
    db.compact_all().unwrap();
    let live: u64 = db.stats().bytes_per_level.iter().sum();
    // 1500 keys * ~80 bytes plus overhead; six generations stacked would be 6x that
    assert!(live < 1500 * 120 * 2, "live bytes {live}");

    for i in 0..1500 {
        db.delete(key(i)).unwrap();
    }
    db.compact_all().unwrap();
    db.compact_all().unwrap();
    let live: u64 = db.stats().bytes_per_level.iter().sum();
    assert!(live < 4096, "tombstones not collected: {live}");
}

#[test]
fn write_stall_does_not_deadlock() {
    let d = tempfile::tempdir().unwrap();
    let opts = Options {
        write_buffer_size: 8 << 10,
        l0_compaction_trigger: 2,
        l0_slowdown_trigger: 3,
        l0_stop_trigger: 4,
        ..common::small_opts()
    };
    let db = lsmdb::Db::open(d.path(), opts).unwrap();
    let wo = WriteOptions { sync: false };
    for i in 0..6000 {
        let mut b = WriteBatch::new();
        b.put(key(i), val(i, 0));
        db.write(b, &wo).unwrap();
    }
    assert_eq!(db.iter().unwrap().count(), 6000);
}
