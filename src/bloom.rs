// MurmurHash64A
pub fn hash64(data: &[u8], seed: u64) -> u64 {
    const M: u64 = 0xc6a4_a793_5bd1_e995;
    let mut h = seed ^ (data.len() as u64).wrapping_mul(M);
    for c in data.chunks(8) {
        let mut k = 0u64;
        for (i, b) in c.iter().enumerate() {
            k |= (*b as u64) << (8 * i);
        }
        if c.len() == 8 {
            k = k.wrapping_mul(M);
            k ^= k >> 47;
            k = k.wrapping_mul(M);
            h ^= k;
        } else {
            h ^= k;
        }
        h = h.wrapping_mul(M);
    }
    h ^= h >> 47;
    h = h.wrapping_mul(M);
    h ^= h >> 47;
    h
}

const SEED: u64 = 0xbc9f_1d34;

#[derive(Default)]
pub struct BloomBuilder {
    hashes: Vec<u64>,
}

impl BloomBuilder {
    pub fn add(&mut self, key: &[u8]) {
        self.hashes.push(hash64(key, SEED));
    }

    // layout: bit array ++ k (1 byte). empty output means "no filter"
    pub fn finish(&self, bits_per_key: usize) -> Vec<u8> {
        if bits_per_key == 0 || self.hashes.is_empty() {
            return Vec::new();
        }
        let k = ((bits_per_key as f64 * 0.69) as usize).clamp(1, 30);
        let nbits = (self.hashes.len() * bits_per_key).max(64);
        let nbytes = nbits.div_ceil(8);
        let nbits = nbytes * 8;
        let mut out = vec![0u8; nbytes + 1];
        for &h0 in &self.hashes {
            let delta = h0.rotate_left(17);
            let mut h = h0;
            for _ in 0..k {
                let bit = (h % nbits as u64) as usize;
                out[bit / 8] |= 1 << (bit % 8);
                h = h.wrapping_add(delta);
            }
        }
        out[nbytes] = k as u8;
        out
    }
}

pub fn may_contain(filter: &[u8], key: &[u8]) -> bool {
    if filter.len() < 2 {
        return true;
    }
    let k = filter[filter.len() - 1];
    if k > 30 {
        // written by some future encoding, don't risk a false negative
        return true;
    }
    let bits = &filter[..filter.len() - 1];
    let nbits = (bits.len() * 8) as u64;
    let h0 = hash64(key, SEED);
    let delta = h0.rotate_left(17);
    let mut h = h0;
    for _ in 0..k {
        let bit = (h % nbits) as usize;
        if bits[bit / 8] & (1 << (bit % 8)) == 0 {
            return false;
        }
        h = h.wrapping_add(delta);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(i: u32) -> Vec<u8> {
        format!("key-{i:08}").into_bytes()
    }

    #[test]
    fn no_false_negatives() {
        let mut b = BloomBuilder::default();
        for i in 0..10_000 {
            b.add(&key(i));
        }
        let f = b.finish(10);
        for i in 0..10_000 {
            assert!(may_contain(&f, &key(i)));
        }
    }

    #[test]
    fn false_positive_rate_is_sane() {
        let mut b = BloomBuilder::default();
        for i in 0..10_000 {
            b.add(&key(i));
        }
        let f = b.finish(10);
        let fp = (10_000..110_000).filter(|&i| may_contain(&f, &key(i))).count();
        // theory says ~1% at 10 bits/key
        assert!(fp < 2_500, "fp = {fp}");
    }

    #[test]
    fn empty_filter_accepts_everything() {
        assert!(may_contain(&[], b"x"));
        assert!(BloomBuilder::default().finish(10).is_empty());
    }
}
