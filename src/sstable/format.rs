use crate::{
    coding::*,
    error::{Error, Result},
};

// "LSMDB001"
pub const MAGIC: u64 = 0x4c53_4d44_4230_3031;
// compression byte + masked crc32 of data++type
pub const BLOCK_TRAILER: usize = 5;
pub const BLOCK_RAW: u8 = 0;
pub const BLOCK_LZ4: u8 = 1;
pub const FOOTER_LEN: usize = 48;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockHandle {
    pub offset: u64,
    pub size: u64,
}

impl BlockHandle {
    pub fn encode(&self, buf: &mut Vec<u8>) {
        put_varint64(buf, self.offset);
        put_varint64(buf, self.size);
    }

    pub fn decode(src: &[u8]) -> Result<BlockHandle> {
        let (offset, n) = get_varint64(src).ok_or_else(|| Error::corruption("bad block handle"))?;
        let (size, _) = get_varint64(&src[n..]).ok_or_else(|| Error::corruption("bad block handle"))?;
        Ok(BlockHandle { offset, size })
    }
}

pub struct Footer {
    pub filter: BlockHandle,
    pub index: BlockHandle,
    pub num_entries: u64,
}

impl Footer {
    pub fn encode(&self) -> [u8; FOOTER_LEN] {
        let mut b = Vec::with_capacity(FOOTER_LEN);
        for v in [
            self.filter.offset,
            self.filter.size,
            self.index.offset,
            self.index.size,
            self.num_entries,
            MAGIC,
        ] {
            put_fixed64(&mut b, v);
        }
        b.try_into().unwrap()
    }

    pub fn decode(src: &[u8]) -> Result<Footer> {
        if src.len() != FOOTER_LEN || read_fixed64(&src[40..]) != MAGIC {
            return Err(Error::corruption("bad sstable footer"));
        }
        let f = |i: usize| read_fixed64(&src[i * 8..]);
        Ok(Footer {
            filter: BlockHandle { offset: f(0), size: f(1) },
            index: BlockHandle { offset: f(2), size: f(3) },
            num_entries: f(4),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn footer_roundtrip() {
        let f = Footer {
            filter: BlockHandle { offset: 1, size: 2 },
            index: BlockHandle { offset: 300, size: 4 },
            num_entries: 99,
        };
        let d = Footer::decode(&f.encode()).unwrap();
        assert_eq!(d.index, f.index);
        assert_eq!(d.num_entries, 99);
    }

    #[test]
    fn bad_magic_rejected() {
        let mut b = Footer {
            filter: BlockHandle { offset: 0, size: 0 },
            index: BlockHandle { offset: 0, size: 0 },
            num_entries: 0,
        }
        .encode();
        b[47] ^= 1;
        assert!(Footer::decode(&b).is_err());
    }
}
