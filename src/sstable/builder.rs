use std::{
    fs::{self, File},
    io::{BufWriter, Write},
    path::{Path, PathBuf},
};

use super::{
    block::BlockBuilder,
    format::{BlockHandle, Footer, BLOCK_LZ4, BLOCK_RAW},
};
use crate::{
    bloom::BloomBuilder,
    coding::{crc_masked, put_fixed32},
    error::Result,
    options::{Compression, Options},
    types::user_key,
};

pub struct TableInfo {
    pub size: u64,
    pub smallest: Vec<u8>,
    pub largest: Vec<u8>,
}

pub struct TableBuilder {
    file: BufWriter<File>,
    path: PathBuf,
    offset: u64,
    data: BlockBuilder,
    index: BlockBuilder,
    bloom: BloomBuilder,
    block_size: usize,
    compression: Compression,
    bits_per_key: usize,
    smallest: Vec<u8>,
    last_key: Vec<u8>,
    entries: u64,
}

fn write_block(file: &mut BufWriter<File>, offset: &mut u64, data: &[u8], kind: u8) -> Result<BlockHandle> {
    let handle = BlockHandle { offset: *offset, size: data.len() as u64 };
    let mut trailer = Vec::with_capacity(5);
    trailer.push(kind);
    let crc = crc_masked(&[data, &trailer]);
    put_fixed32(&mut trailer, crc);
    file.write_all(data)?;
    file.write_all(&trailer)?;
    *offset += (data.len() + trailer.len()) as u64;
    Ok(handle)
}

impl TableBuilder {
    pub fn new(path: &Path, opts: &Options) -> Result<Self> {
        let file = File::options().write(true).create_new(true).open(path)?;
        Ok(TableBuilder {
            file: BufWriter::with_capacity(256 << 10, file),
            path: path.to_path_buf(),
            offset: 0,
            data: BlockBuilder::new(opts.block_restart_interval),
            // one restart per entry so the index is a pure binary search
            index: BlockBuilder::new(1),
            bloom: BloomBuilder::default(),
            block_size: opts.block_size,
            compression: opts.compression,
            bits_per_key: opts.bloom_bits_per_key,
            smallest: Vec::new(),
            last_key: Vec::new(),
            entries: 0,
        })
    }

    pub fn file_size(&self) -> u64 {
        self.offset + self.data.size_estimate() as u64
    }

    pub fn last_key(&self) -> &[u8] {
        &self.last_key
    }

    pub fn add(&mut self, ikey: &[u8], value: &[u8]) -> Result<()> {
        if self.entries == 0 {
            self.smallest = ikey.to_vec();
        }
        // filter is keyed by user key; versions of one key are adjacent
        if self.entries == 0 || user_key(ikey) != user_key(&self.last_key) {
            self.bloom.add(user_key(ikey));
        }
        self.data.add(ikey, value);
        self.last_key.clear();
        self.last_key.extend_from_slice(ikey);
        self.entries += 1;
        if self.data.size_estimate() >= self.block_size {
            self.flush_data()?;
        }
        Ok(())
    }

    fn flush_data(&mut self) -> Result<()> {
        if self.data.is_empty() {
            return Ok(());
        }
        let raw = self.data.finish();
        let h = match self.compression {
            Compression::Lz4 => {
                let packed = lz4_flex::block::compress_prepend_size(raw);
                // not worth the decode cost unless it saves at least 1/8
                if packed.len() < raw.len() - raw.len() / 8 {
                    write_block(&mut self.file, &mut self.offset, &packed, BLOCK_LZ4)?
                } else {
                    write_block(&mut self.file, &mut self.offset, raw, BLOCK_RAW)?
                }
            }
            Compression::None => write_block(&mut self.file, &mut self.offset, raw, BLOCK_RAW)?,
        };
        self.data.reset();
        let mut enc = Vec::new();
        h.encode(&mut enc);
        // index key is the block's last key, so seek(target) finds the one block that can hold it
        self.index.add(&self.last_key, &enc);
        Ok(())
    }

    pub fn finish(mut self) -> Result<TableInfo> {
        self.flush_data()?;
        let filter = self.bloom.finish(self.bits_per_key);
        let filter_h = write_block(&mut self.file, &mut self.offset, &filter, BLOCK_RAW)?;
        let index_h = write_block(&mut self.file, &mut self.offset, self.index.finish(), BLOCK_RAW)?;
        let footer = Footer { filter: filter_h, index: index_h, num_entries: self.entries };
        self.file.write_all(&footer.encode())?;
        self.offset += super::format::FOOTER_LEN as u64;
        self.file.flush()?;
        self.file.get_ref().sync_all()?;
        Ok(TableInfo { size: self.offset, smallest: self.smallest, largest: self.last_key })
    }

    pub fn abandon(self) {
        let path = self.path.clone();
        drop(self);
        let _ = fs::remove_file(path);
    }
}
