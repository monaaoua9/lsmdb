mod common;
use common::*;
use lsmdb::{Db, Options};
use std::collections::BTreeMap;

// random writes with flushes/compactions, then check checkpoint, read-only
// reopen and reverse scans all agree with a plain map
#[test]
fn checkpoint_and_readonly_match_model() {
    for seed in 1..=3u64 {
        let d = tempfile::tempdir().unwrap();
        let live = d.path().join("live");
        let mut model: BTreeMap<Vec<u8>, Vec<u8>> = BTreeMap::new();
        let mut rng = Rng(seed * 0x2545f4914f6cdd1d);
        let db = open(&live);

        for round in 0..5 {
            for _ in 0..250 {
                let k = key(rng.below(200) as u32);
                if rng.below(3) == 0 {
                    db.delete(k.clone()).unwrap();
                    model.remove(&k);
                } else {
                    let v = val(rng.below(500) as u32, round);
                    db.put(k.clone(), v.clone()).unwrap();
                    model.insert(k, v);
                }
            }
            match rng.below(3) {
                0 => db.flush().unwrap(),
                1 => db.compact_all().unwrap(),
                _ => {}
            }
        }

        let cp = d.path().join("cp");
        db.checkpoint(&cp).unwrap();
        let want: Vec<_> = model.iter().map(|(k, v)| (k.clone(), v.clone())).collect();

        let copy = Db::open(&cp, Options { read_only: true, ..small_opts() }).unwrap();
        let got: Vec<_> = copy.iter().unwrap().map(|r| r.unwrap()).collect();
        assert_eq!(got, want, "seed {seed}");
        let mut back: Vec<_> = copy.iter().unwrap().rev().map(|r| r.unwrap()).collect();
        back.reverse();
        assert_eq!(back, want, "seed {seed} reverse");
        copy.verify_integrity().unwrap();
        drop(copy);

        drop(db);
        let ro = Db::open(&live, Options { read_only: true, ..small_opts() }).unwrap();
        let got: Vec<_> = ro.iter().unwrap().map(|r| r.unwrap()).collect();
        assert_eq!(got, want, "seed {seed} read-only live");
    }
}
