use std::sync::Arc;

use crate::{
    coding::*,
    error::{Error, Result},
    iter::InternalIterator,
    types::cmp_ikey,
};
use std::cmp::Ordering;

/// anything that can back a block: an mmap'd file, or a Vec in tests
pub type Backing = Arc<dyn AsRef<[u8]> + Send + Sync>;

// entry: shared u32v | unshared u32v | value_len u32v | key delta | value
// tail:  restart offsets (u32 each) | restart count u32
pub struct BlockBuilder {
    buf: Vec<u8>,
    restarts: Vec<u32>,
    counter: usize,
    interval: usize,
    last_key: Vec<u8>,
}

impl BlockBuilder {
    pub fn new(interval: usize) -> Self {
        BlockBuilder {
            buf: Vec::new(),
            restarts: vec![0],
            counter: 0,
            interval: interval.max(1),
            last_key: Vec::new(),
        }
    }

    pub fn add(&mut self, key: &[u8], value: &[u8]) {
        let mut shared = 0;
        if self.counter < self.interval {
            shared = self.last_key.iter().zip(key).take_while(|(a, b)| a == b).count();
        } else {
            self.restarts.push(self.buf.len() as u32);
            self.counter = 0;
        }
        put_varint32(&mut self.buf, shared as u32);
        put_varint32(&mut self.buf, (key.len() - shared) as u32);
        put_varint32(&mut self.buf, value.len() as u32);
        self.buf.extend_from_slice(&key[shared..]);
        self.buf.extend_from_slice(value);

        self.last_key.truncate(shared);
        self.last_key.extend_from_slice(&key[shared..]);
        self.counter += 1;
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    pub fn size_estimate(&self) -> usize {
        self.buf.len() + self.restarts.len() * 4 + 4
    }

    pub fn finish(&mut self) -> &[u8] {
        for r in &self.restarts {
            put_fixed32(&mut self.buf, *r);
        }
        put_fixed32(&mut self.buf, self.restarts.len() as u32);
        &self.buf
    }

    pub fn reset(&mut self) {
        self.buf.clear();
        self.restarts.clear();
        self.restarts.push(0);
        self.counter = 0;
        self.last_key.clear();
    }
}

#[derive(Clone)]
pub struct Block {
    backing: Backing,
    start: usize,
    end: usize,
    // relative to start
    restarts_off: usize,
    n_restarts: usize,
}

impl Block {
    pub fn new(backing: Backing, start: usize, end: usize) -> Result<Block> {
        let raw: &dyn AsRef<[u8]> = &*backing;
        let data = raw.as_ref().get(start..end).ok_or_else(|| Error::corruption("block out of range"))?;
        if data.len() < 4 {
            return Err(Error::corruption("block too small"));
        }
        let n = read_fixed32(&data[data.len() - 4..]) as usize;
        if n == 0 || n > (data.len() - 4) / 4 {
            return Err(Error::corruption("bad restart count"));
        }
        let restarts_off = data.len() - 4 - 4 * n;
        Ok(Block { backing, start, end, restarts_off, n_restarts: n })
    }

    fn data(&self) -> &[u8] {
        let raw: &dyn AsRef<[u8]> = &*self.backing;
        &raw.as_ref()[self.start..self.end]
    }

    fn restart_point(&self, i: usize) -> usize {
        read_fixed32(&self.data()[self.restarts_off + 4 * i..]) as usize
    }

    pub fn iter(&self) -> BlockIter {
        BlockIter {
            block: self.clone(),
            key: Vec::new(),
            voff: 0,
            vlen: 0,
            cur_off: 0,
            next_off: 0,
            valid: false,
            err: None,
        }
    }
}

pub struct BlockIter {
    block: Block,
    key: Vec<u8>,
    voff: usize,
    vlen: usize,
    // where the current entry starts; prev() needs it to find the entry before
    cur_off: usize,
    next_off: usize,
    valid: bool,
    err: Option<Error>,
}

impl BlockIter {
    fn fail(&mut self) {
        self.valid = false;
        self.err = Some(Error::corruption("bad block entry"));
    }

    fn parse_next(&mut self) {
        let limit = self.block.restarts_off;
        if self.next_off >= limit {
            self.valid = false;
            return;
        }
        self.cur_off = self.next_off;
        let src = &self.block.data()[self.next_off..limit];
        let Some((shared, n1)) = get_varint32(src) else { return self.fail() };
        let Some((unshared, n2)) = get_varint32(&src[n1..]) else { return self.fail() };
        let Some((vlen, n3)) = get_varint32(&src[n1 + n2..]) else { return self.fail() };
        let (shared, unshared, vlen) = (shared as usize, unshared as usize, vlen as usize);
        let hdr = n1 + n2 + n3;
        if shared > self.key.len() || src.len() < hdr + unshared + vlen {
            return self.fail();
        }
        self.key.truncate(shared);
        self.key.extend_from_slice(&src[hdr..hdr + unshared]);
        self.voff = self.next_off + hdr + unshared;
        self.vlen = vlen;
        self.next_off = self.voff + vlen;
        self.valid = true;
    }

    fn seek_to_restart(&mut self, i: usize) {
        self.key.clear();
        self.next_off = self.block.restart_point(i);
        self.valid = false;
    }

    // restart entries have shared == 0 so the full key sits right there
    fn restart_key(&self, i: usize) -> Option<&[u8]> {
        let off = self.block.restart_point(i);
        let src = self.block.data().get(off..self.block.restarts_off)?;
        let (shared, n1) = get_varint32(src)?;
        let (unshared, n2) = get_varint32(&src[n1..])?;
        let (_, n3) = get_varint32(&src[n1 + n2..])?;
        let hdr = n1 + n2 + n3;
        if shared != 0 || src.len() < hdr + unshared as usize {
            return None;
        }
        Some(&src[hdr..hdr + unshared as usize])
    }
}

impl InternalIterator for BlockIter {
    fn valid(&self) -> bool {
        self.valid
    }

    fn seek_to_first(&mut self) {
        self.seek_to_restart(0);
        self.parse_next();
    }

    fn seek_to_last(&mut self) {
        self.seek_to_restart(self.block.n_restarts - 1);
        loop {
            self.parse_next();
            if !self.valid || self.next_off >= self.block.restarts_off {
                break;
            }
        }
    }

    fn prev(&mut self) {
        let original = self.cur_off;
        // number of restart points strictly before the current entry
        let (mut lo, mut hi) = (0, self.block.n_restarts);
        while lo < hi {
            let mid = (lo + hi) / 2;
            if self.block.restart_point(mid) < original {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        if lo == 0 {
            self.valid = false;
            return;
        }
        self.seek_to_restart(lo - 1);
        loop {
            self.parse_next();
            if !self.valid || self.next_off >= original {
                break;
            }
        }
    }

    fn seek(&mut self, target: &[u8]) {
        let (mut left, mut right) = (0, self.block.n_restarts - 1);
        while left < right {
            let mid = (left + right).div_ceil(2);
            let Some(k) = self.restart_key(mid) else { return self.fail() };
            if cmp_ikey(k, target) == Ordering::Less {
                left = mid;
            } else {
                right = mid - 1;
            }
        }
        self.seek_to_restart(left);
        loop {
            self.parse_next();
            if !self.valid || cmp_ikey(&self.key, target) != Ordering::Less {
                break;
            }
        }
    }

    fn next(&mut self) {
        self.parse_next();
    }

    fn key(&self) -> &[u8] {
        &self.key
    }

    fn value(&self) -> &[u8] {
        &self.block.data()[self.voff..self.voff + self.vlen]
    }

    fn status(&self) -> Result<()> {
        match &self.err {
            Some(e) => Err(e.clone()),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{make_ikey, ValueKind};

    fn build(n: u32, interval: usize) -> Block {
        let mut b = BlockBuilder::new(interval);
        for i in 0..n {
            b.add(&make_ikey(format!("key{i:05}").as_bytes(), 1, ValueKind::Put), format!("v{i}").as_bytes());
        }
        let bytes = b.finish().to_vec();
        let len = bytes.len();
        Block::new(Arc::new(bytes), 0, len).unwrap()
    }

    #[test]
    fn scan_all() {
        let blk = build(100, 16);
        let mut it = blk.iter();
        it.seek_to_first();
        for i in 0..100 {
            assert!(it.valid());
            assert_eq!(it.value(), format!("v{i}").as_bytes());
            it.next();
        }
        assert!(!it.valid());
        assert!(it.status().is_ok());
    }

    #[test]
    fn reverse_scan_all() {
        for interval in [1, 3, 16] {
            let blk = build(100, interval);
            let mut it = blk.iter();
            it.seek_to_last();
            for i in (0..100).rev() {
                assert!(it.valid(), "interval {interval} at {i}");
                assert_eq!(it.value(), format!("v{i}").as_bytes());
                it.prev();
            }
            assert!(!it.valid());
            assert!(it.status().is_ok());
        }
    }

    #[test]
    fn prev_after_seek_and_direction_flips() {
        let blk = build(50, 4);
        let mut it = blk.iter();
        it.seek(&make_ikey(b"key00020", 1, ValueKind::Put));
        assert_eq!(it.value(), b"v20");
        it.prev();
        assert_eq!(it.value(), b"v19");
        it.next();
        it.next();
        assert_eq!(it.value(), b"v21");
        it.prev();
        it.prev();
        assert_eq!(it.value(), b"v19");

        it.seek_to_first();
        it.prev();
        assert!(!it.valid());
    }

    #[test]
    fn seek_every_key() {
        for interval in [1, 4, 16] {
            let blk = build(200, interval);
            let mut it = blk.iter();
            for i in 0..200 {
                it.seek(&make_ikey(format!("key{i:05}").as_bytes(), 1, ValueKind::Put));
                assert!(it.valid());
                assert_eq!(it.value(), format!("v{i}").as_bytes());
            }
        }
    }

    #[test]
    fn seek_between_and_past_end() {
        let blk = build(10, 4);
        let mut it = blk.iter();
        it.seek(&make_ikey(b"key00003x", 1, ValueKind::Put));
        assert_eq!(it.value(), b"v4");
        it.seek(&make_ikey(b"zzz", 1, ValueKind::Put));
        assert!(!it.valid());
        it.seek(&make_ikey(b"a", 1, ValueKind::Put));
        assert_eq!(it.value(), b"v0");
    }

    #[test]
    fn garbage_block_is_rejected() {
        let v: Vec<u8> = vec![0xff; 3];
        assert!(Block::new(Arc::new(v), 0, 3).is_err());
        let mut v = vec![0u8; 8];
        v[4..].copy_from_slice(&1000u32.to_le_bytes());
        assert!(Block::new(Arc::new(v), 0, 8).is_err());
    }
}
