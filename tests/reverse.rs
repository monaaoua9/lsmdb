mod common;
use common::*;
use std::collections::BTreeMap;
use std::ops::Bound::{Excluded, Included, Unbounded};

type Model = BTreeMap<Vec<u8>, Vec<u8>>;

fn rev_all(it: lsmdb::DbIter) -> Vec<Vec<u8>> {
    it.rev().map(|r| r.unwrap().0).collect()
}

#[test]
fn reverse_full_scan_across_all_components() {
    let d = tempfile::tempdir().unwrap();
    let db = open(d.path());
    for i in 0..2000 {
        db.put(key(i), val(i, 0)).unwrap();
    }
    db.flush().unwrap();
    db.compact_all().unwrap();
    for i in (0..2000).step_by(3) {
        db.put(key(i), val(i, 1)).unwrap();
    }
    db.flush().unwrap();
    for i in (0..2000).step_by(7) {
        db.delete(key(i)).unwrap();
    }
    // some stuff only in the memtable
    for i in 2000..2050 {
        db.put(key(i), val(i, 0)).unwrap();
    }

    let fwd: Vec<_> = db.iter().unwrap().map(|r| r.unwrap().0).collect();
    let mut back = rev_all(db.iter().unwrap());
    back.reverse();
    assert_eq!(fwd, back);
    assert!(fwd.len() == 2000 - 286 + 50);
}

#[test]
fn reverse_respects_bounds_and_snapshots() {
    let d = tempfile::tempdir().unwrap();
    let db = open(d.path());
    for i in 0..100 {
        db.put(key(i), "old").unwrap();
    }
    let snap = db.snapshot();
    for i in 0..100 {
        db.put(key(i), "new").unwrap();
    }
    db.delete(key(50)).unwrap();

    let got: Vec<_> = db.range(Some(&key(10)), Some(&key(60))).unwrap().rev().map(|r| r.unwrap()).collect();
    assert_eq!(got.len(), 49);
    assert_eq!(got[0].0, key(59));
    assert_eq!(got.last().unwrap().0, key(10));
    assert!(got.iter().all(|(_, v)| v == b"new"));

    let old: Vec<_> = snap.range(Some(&key(10)), Some(&key(60))).unwrap().rev().map(|r| r.unwrap()).collect();
    assert_eq!(old.len(), 50);
    assert!(old.iter().all(|(_, v)| v == b"old"));
}

#[test]
fn last_key_value_and_empty() {
    let d = tempfile::tempdir().unwrap();
    let db = open(d.path());
    assert!(db.last_key_value().unwrap().is_none());
    assert_eq!(db.iter().unwrap().rev().count(), 0);
    db.put("a", "1").unwrap();
    db.put("z", "2").unwrap();
    db.delete("z").unwrap();
    assert_eq!(db.last_key_value().unwrap().unwrap().0, b"a");
    db.flush().unwrap();
    db.put("m", "3").unwrap();
    assert_eq!(db.last_key_value().unwrap().unwrap().0, b"m");
}

#[test]
fn seek_for_prev_basics() {
    let d = tempfile::tempdir().unwrap();
    let db = open(d.path());
    for i in (0..100).step_by(10) {
        db.put(key(i), "v").unwrap();
    }
    db.flush().unwrap();
    let mut it = db.iter().unwrap();
    it.seek_for_prev(&key(35));
    assert_eq!(it.key(), &key(30)[..]);
    it.seek_for_prev(&key(30));
    assert_eq!(it.key(), &key(30)[..]);
    it.seek_for_prev(&key(5000));
    assert_eq!(it.key(), &key(90)[..]);
    it.seek_for_prev(b"");
    assert!(!it.valid());

    let mut bounded = db.range(Some(&key(20)), Some(&key(60))).unwrap();
    bounded.seek_for_prev(&key(99));
    assert_eq!(bounded.key(), &key(50)[..]);
    bounded.seek_for_prev(&key(15));
    assert!(!bounded.valid());
}

#[test]
fn direction_switches_match_a_model() {
    for seed in 1..=4u64 {
        let d = tempfile::tempdir().unwrap();
        let db = open(d.path());
        let mut rng = Rng(seed * 0x9e3779b97f4a7c15);
        let mut model = Model::new();

        for round in 0..6 {
            for _ in 0..300 {
                let k = key(rng.below(150) as u32);
                if rng.below(4) == 0 {
                    db.delete(k.clone()).unwrap();
                    model.remove(&k);
                } else {
                    let v = val(rng.below(1000) as u32, round);
                    db.put(k.clone(), v.clone()).unwrap();
                    model.insert(k, v);
                }
            }
            match round % 3 {
                0 => db.flush().unwrap(),
                1 => db.compact_all().unwrap(),
                _ => {}
            }

            let lo = (rng.below(3) == 0).then(|| key(rng.below(150) as u32));
            let hi = (rng.below(3) == 0).then(|| key(rng.below(150) as u32));
            if let (Some(l), Some(h)) = (&lo, &hi) {
                if l >= h {
                    continue;
                }
            }
            let range = |m: &Model| -> Vec<Vec<u8>> {
                let l = lo.as_ref().map_or(Unbounded, |l| Included(l.clone()));
                let h = hi.as_ref().map_or(Unbounded, |h| Excluded(h.clone()));
                m.range((l, h)).map(|(k, _)| k.clone()).collect()
            };
            let keys = range(&model);

            let mut it = db.range(lo.as_deref(), hi.as_deref()).unwrap();
            let mut pos: Option<usize> = if keys.is_empty() { None } else { Some(0) };
            assert_eq!(it.valid(), pos.is_some());

            for step in 0..400 {
                match rng.below(6) {
                    0 | 1 => {
                        it.advance();
                        pos = match pos {
                            Some(p) if p + 1 < keys.len() => Some(p + 1),
                            _ => None,
                        };
                    }
                    2 | 3 => {
                        it.retreat();
                        pos = match pos {
                            Some(p) if p > 0 => Some(p - 1),
                            _ => None,
                        };
                    }
                    4 => {
                        let t = key(rng.below(160) as u32);
                        it.seek(&t);
                        pos = keys.iter().position(|k| *k >= t);
                    }
                    _ => {
                        let t = key(rng.below(160) as u32);
                        it.seek_for_prev(&t);
                        pos = keys.iter().rposition(|k| *k <= t);
                    }
                }
                assert_eq!(it.valid(), pos.is_some(), "seed {seed} round {round} step {step}");
                if let Some(p) = pos {
                    assert_eq!(it.key(), &keys[p][..], "seed {seed} round {round} step {step}");
                    assert_eq!(it.value(), &model[&keys[p]][..]);
                }
                it.status().unwrap();
            }
        }
    }
}
