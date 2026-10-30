use std::{
    fs::File,
    path::Path,
    sync::atomic::{AtomicU64, Ordering::Relaxed},
    sync::Arc,
};

use memmap2::Mmap;

use super::{
    block::{Backing, Block, BlockIter},
    format::{BlockHandle, Footer, BLOCK_LZ4, BLOCK_RAW, BLOCK_TRAILER, FOOTER_LEN},
};
use crate::{
    bloom,
    cache::BlockCache,
    coding::{crc_masked, read_fixed32},
    error::{Error, Result},
    iter::InternalIterator,
    metrics::Metrics,
    types::{parse_ikey, Hit, ValueKind},
};

const MAX_BLOCK: usize = 256 << 20;

// cache keys must not collide across tables, and file numbers can be reused after a crash
static NEXT_TABLE_ID: AtomicU64 = AtomicU64::new(1);

/// Immutable SSTable served straight out of an mmap: blocks are slices of
/// the mapping, so a read costs a page fault at worst and no copy into a
/// user-space buffer. The OS page cache doubles as our block cache.
pub struct Table {
    backing: Backing,
    len: usize,
    index: Block,
    filter: (usize, usize),
    verify: bool,
    id: u64,
    cache: Option<Arc<BlockCache>>,
}

impl Table {
    pub fn open(path: &Path, verify: bool, cache: Option<Arc<BlockCache>>) -> Result<Arc<Table>> {
        let file = File::open(path)?;
        let len = file.metadata()?.len() as usize;
        if len < FOOTER_LEN {
            return Err(Error::corruption(format!("{} too short", path.display())));
        }
        // SAFETY: sstables are immutable once written and we never truncate
        // or rewrite them; files are removed only after the last handle
        // (this mapping included) is dropped. External tampering while the
        // db is open is outside the contract, same as any mmap-based engine.
        let mmap = unsafe { Mmap::map(&file)? };
        let backing: Backing = Arc::new(mmap);

        let raw: &dyn AsRef<[u8]> = &*backing;
        let footer = Footer::decode(&raw.as_ref()[len - FOOTER_LEN..])?;
        let index = read_block(&backing, len, verify, footer.index, None)?;
        check_region(raw.as_ref(), len, verify, footer.filter)?;
        Ok(Arc::new(Table {
            backing,
            len,
            index,
            filter: (footer.filter.offset as usize, footer.filter.size as usize),
            verify,
            id: NEXT_TABLE_ID.fetch_add(1, Relaxed),
            cache,
        }))
    }

    fn bytes(&self) -> &[u8] {
        let raw: &dyn AsRef<[u8]> = &*self.backing;
        raw.as_ref()
    }

    fn read_block(&self, h: BlockHandle) -> Result<Block> {
        read_block(&self.backing, self.len, self.verify, h, self.cache.as_deref().map(|c| (c, self.id)))
    }

    pub fn get(&self, ukey: &[u8], seek: &[u8], m: &Metrics) -> Result<Option<Hit>> {
        let filter = &self.bytes()[self.filter.0..self.filter.0 + self.filter.1];
        m.bloom_checked.fetch_add(1, Relaxed);
        if !bloom::may_contain(filter, ukey) {
            m.bloom_skipped.fetch_add(1, Relaxed);
            return Ok(None);
        }
        let mut idx = self.index.iter();
        idx.seek(seek);
        idx.status()?;
        if !idx.valid() {
            return Ok(None);
        }
        let h = BlockHandle::decode(idx.value())?;
        m.block_reads.fetch_add(1, Relaxed);
        let mut it = self.read_block(h)?.iter();
        it.seek(seek);
        it.status()?;
        if !it.valid() {
            return Ok(None);
        }
        let Some((u, seq, kind)) = parse_ikey(it.key()) else {
            return Err(Error::corruption("bad internal key"));
        };
        if u != ukey {
            return Ok(None);
        }
        let value = (kind == ValueKind::Put).then(|| it.value().to_vec());
        Ok(Some(Hit { seq, value }))
    }

    pub fn iter(self: &Arc<Self>) -> TableIterator {
        TableIterator { table: self.clone(), index: self.index.iter(), data: None, err: None }
    }
}

fn check_region(bytes: &[u8], len: usize, verify: bool, h: BlockHandle) -> Result<()> {
    let (off, size) = (h.offset as usize, h.size as usize);
    let end = off.checked_add(size).ok_or_else(|| Error::corruption("block overflow"))?;
    if end + BLOCK_TRAILER > len - FOOTER_LEN {
        return Err(Error::corruption("block past end of file"));
    }
    if verify && crc_masked(&[&bytes[off..end + 1]]) != read_fixed32(&bytes[end + 1..]) {
        return Err(Error::corruption("block checksum mismatch"));
    }
    Ok(())
}

fn read_block(
    backing: &Backing,
    len: usize,
    verify: bool,
    h: BlockHandle,
    cache: Option<(&BlockCache, u64)>,
) -> Result<Block> {
    let raw: &dyn AsRef<[u8]> = &**backing;
    let off = h.offset as usize;
    if let Some((c, id)) = cache {
        if let Some(plain) = c.get(id, h.offset) {
            let n = plain.len();
            return Block::new(plain, 0, n);
        }
    }
    check_region(raw.as_ref(), len, verify, h)?;
    let end = off + h.size as usize;
    match raw.as_ref()[end] {
        BLOCK_RAW => Block::new(backing.clone(), off, end),
        BLOCK_LZ4 => {
            let src = &raw.as_ref()[off..end];
            // size prefix is covered by the crc, but verify_checksums can be off
            let want = src.get(..4).map(read_fixed32).ok_or_else(|| Error::corruption("short lz4 block"))?;
            if want as usize > MAX_BLOCK {
                return Err(Error::corruption("absurd lz4 block size"));
            }
            let plain = lz4_flex::block::decompress_size_prepended(src)
                .map_err(|e| Error::corruption(format!("lz4: {e}")))?;
            let n = plain.len();
            let plain = Arc::new(plain);
            if let Some((c, id)) = cache {
                c.insert(id, h.offset, plain.clone());
            }
            Block::new(plain, 0, n)
        }
        t => Err(Error::corruption(format!("unknown block type {t}"))),
    }
}

pub struct TableIterator {
    table: Arc<Table>,
    index: BlockIter,
    data: Option<BlockIter>,
    err: Option<Error>,
}

impl TableIterator {
    fn open_data_block(&mut self) {
        self.data = None;
        if !self.index.valid() {
            return;
        }
        let res = BlockHandle::decode(self.index.value()).and_then(|h| self.table.read_block(h));
        match res {
            Ok(b) => self.data = Some(b.iter()),
            Err(e) => self.err = Some(e),
        }
    }

    fn skip_empty_forward(&mut self) {
        loop {
            match &self.data {
                Some(d) if d.valid() => return,
                Some(d) => {
                    if let Err(e) = d.status() {
                        self.err = Some(e);
                        self.data = None;
                        return;
                    }
                }
                None if self.err.is_some() => return,
                None => {}
            }
            if !self.index.valid() {
                self.data = None;
                return;
            }
            self.index.next();
            self.open_data_block();
            if let Some(d) = &mut self.data {
                d.seek_to_first();
            }
        }
    }
}

impl TableIterator {
    fn skip_empty_backward(&mut self) {
        loop {
            match &self.data {
                Some(d) if d.valid() => return,
                Some(d) => {
                    if let Err(e) = d.status() {
                        self.err = Some(e);
                        self.data = None;
                        return;
                    }
                }
                None if self.err.is_some() => return,
                None => {}
            }
            if !self.index.valid() {
                self.data = None;
                return;
            }
            self.index.prev();
            self.open_data_block();
            if let Some(d) = &mut self.data {
                d.seek_to_last();
            }
        }
    }
}

impl InternalIterator for TableIterator {
    fn valid(&self) -> bool {
        self.data.as_ref().is_some_and(|d| d.valid())
    }

    fn seek_to_first(&mut self) {
        self.index.seek_to_first();
        self.open_data_block();
        if let Some(d) = &mut self.data {
            d.seek_to_first();
        }
        self.skip_empty_forward();
    }

    fn seek_to_last(&mut self) {
        self.index.seek_to_last();
        self.open_data_block();
        if let Some(d) = &mut self.data {
            d.seek_to_last();
        }
        self.skip_empty_backward();
    }

    fn prev(&mut self) {
        if let Some(d) = &mut self.data {
            d.prev();
        }
        self.skip_empty_backward();
    }

    fn seek(&mut self, target: &[u8]) {
        self.index.seek(target);
        self.open_data_block();
        if let Some(d) = &mut self.data {
            d.seek(target);
        }
        self.skip_empty_forward();
    }

    fn next(&mut self) {
        if let Some(d) = &mut self.data {
            d.next();
        }
        self.skip_empty_forward();
    }

    fn key(&self) -> &[u8] {
        self.data.as_ref().unwrap().key()
    }

    fn value(&self) -> &[u8] {
        self.data.as_ref().unwrap().value()
    }

    fn status(&self) -> Result<()> {
        if let Some(e) = &self.err {
            return Err(e.clone());
        }
        self.index.status()
    }
}
