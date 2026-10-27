use std::sync::Arc;

use crate::{
    options::Options,
    version::{FileMeta, Version, NUM_LEVELS},
};

pub struct Compaction {
    pub level: usize,
    pub inputs: [Vec<Arc<FileMeta>>; 2],
    pub version: Arc<Version>,
}

fn max_bytes(level: usize, o: &Options) -> u64 {
    o.level_base_bytes * o.level_multiplier.pow(level as u32 - 1)
}

// (level, score); score >= 1 means the level wants compacting
fn best_level(v: &Version, o: &Options) -> (usize, f64) {
    let mut best = (0, v.num_files(0) as f64 / o.l0_compaction_trigger as f64);
    for level in 1..NUM_LEVELS - 1 {
        let score = v.level_bytes(level) as f64 / max_bytes(level, o) as f64;
        if score > best.1 {
            best = (level, score);
        }
    }
    best
}

pub fn needs_compaction(v: &Version, o: &Options) -> bool {
    best_level(v, o).1 >= 1.0
}

/// `force` compacts every file of that level regardless of score
pub fn pick(v: &Arc<Version>, ptrs: &[Vec<u8>], o: &Options, force: Option<usize>) -> Option<Compaction> {
    let level = match force {
        Some(l) if l < NUM_LEVELS - 1 && !v.levels[l].is_empty() => l,
        Some(_) => return None,
        None => {
            let (l, score) = best_level(v, o);
            if score < 1.0 {
                return None;
            }
            l
        }
    };

    let inputs0: Vec<Arc<FileMeta>> = if level == 0 || force.is_some() {
        v.levels[level].clone()
    } else {
        // round-robin through the keyspace so one hot range doesn't starve the rest
        let files = &v.levels[level];
        let f = files
            .iter()
            .find(|f| ptrs[level].is_empty() || crate::types::cmp_ikey(&f.largest, &ptrs[level]).is_gt())
            .unwrap_or(&files[0]);
        vec![f.clone()]
    };

    let lo = inputs0.iter().map(|f| f.smallest_user()).min()?;
    let hi = inputs0.iter().map(|f| f.largest_user()).max()?;
    let inputs1 = v.overlapping(level + 1, lo, hi);
    Some(Compaction { level, inputs: [inputs0, inputs1], version: v.clone() })
}

impl Compaction {
    pub fn is_trivial_move(&self) -> bool {
        self.inputs[0].len() == 1 && self.inputs[1].is_empty()
    }

    // true if no deeper level can hold an older version of this key, which
    // is what makes dropping a tombstone safe
    pub fn is_base_level_for_key(&self, ukey: &[u8]) -> bool {
        !self.version.key_may_exist_below(self.level + 1, ukey)
    }
}
