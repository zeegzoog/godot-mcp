//! The uid in the header of a binary scene or resource (`.scn`, `.res`).
//!
//! Godot's binary saver writes `RSRC`, then the header: endianness, real size, engine version,
//! format version, the resource class, the import metadata offset, format flags and, when the
//! flags carry `FORMAT_FLAG_UIDS`, the uid as a 64-bit id (`-1` for none). A file saved with
//! `ResourceSaver.FLAG_COMPRESS` is a `FileAccessCompressed` stream instead: `RSCC`, the mode
//! (2 for zstd, the one Godot uses), the block size, the total size, each block's compressed
//! size, then the blocks. `RSCC` stands in for `RSRC`: the first block starts with the field
//! after the magic.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

const FORMAT_FLAG_UIDS: u32 = 2;
const MODE_ZSTD: u32 = 2;
/// Bytes read from an uncompressed file, and the most a header can take: a class name of up to
/// 4 KiB comes before the uid.
const HEAD_BYTES: usize = 4096 + 64;
/// Godot's blocks are 4 KiB; a larger value is not a file this module understands.
const MAX_BLOCK: u32 = 1 << 20;

/// What the header of a binary resource says about its uid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BinaryUid {
    /// The uid, as `uid://` text.
    Uid(String),
    /// A readable header without a uid.
    Missing,
    /// Not a binary resource this module can read (another compression mode, a damaged file).
    Unreadable,
}

/// The uid in the header of the binary resource `file`.
pub fn binary_uid(file: &Path) -> BinaryUid {
    let Ok(mut opened) = File::open(file) else {
        return BinaryUid::Unreadable;
    };
    let mut magic = [0_u8; 4];
    if opened.read_exact(&mut magic).is_err() {
        return BinaryUid::Unreadable;
    }
    let head = match &magic {
        b"RSRC" => {
            let mut head = magic.to_vec();
            let _ = (&mut opened).take(HEAD_BYTES as u64).read_to_end(&mut head);
            head
        }
        b"RSCC" => match first_block(&mut opened) {
            Some(head) => head,
            None => return BinaryUid::Unreadable,
        },
        _ => return BinaryUid::Unreadable,
    };
    parse_header(&head)
}

/// The decompressed first block of a `FileAccessCompressed` stream, read after its magic.
fn first_block(file: &mut File) -> Option<Vec<u8>> {
    let mut fields = [0_u8; 12];
    file.read_exact(&mut fields).ok()?;
    let word = |at: usize| {
        u32::from_le_bytes([fields[at], fields[at + 1], fields[at + 2], fields[at + 3]])
    };
    let (mode, block_size, total) = (word(0), word(4), word(8));
    if mode != MODE_ZSTD || block_size == 0 || block_size > MAX_BLOCK || total == 0 {
        return None;
    }
    let blocks = total.div_ceil(block_size);
    let mut first_size = [0_u8; 4];
    file.read_exact(&mut first_size).ok()?;
    let first_size = u32::from_le_bytes(first_size);
    if first_size == 0 || first_size > MAX_BLOCK {
        return None;
    }
    // The other block sizes come before the data. Skipped by seeking: a damaged header can promise
    // billions of them, which must not become an allocation.
    file.seek(SeekFrom::Current(4 * (i64::from(blocks) - 1)))
        .ok()?;
    let mut compressed = vec![0_u8; first_size as usize];
    file.read_exact(&mut compressed).ok()?;
    let mut decoder = ruzstd::decoding::StreamingDecoder::new(compressed.as_slice()).ok()?;
    let mut head = b"RSRC".to_vec();
    decoder
        .by_ref()
        .take(u64::from(block_size))
        .read_to_end(&mut head)
        .ok()?;
    (head.len() > 4).then_some(head)
}

fn parse_header(head: &[u8]) -> BinaryUid {
    parse(head).unwrap_or(BinaryUid::Unreadable)
}

fn parse(head: &[u8]) -> Option<BinaryUid> {
    let mut at = 4;
    let mut take = |len: usize| -> Option<&[u8]> {
        let bytes = head.get(at..at + len)?;
        at += len;
        Some(bytes)
    };
    let big_endian = u32_le(take(4)?) != 0;
    let u32_of = |bytes: &[u8]| {
        if big_endian {
            u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
        } else {
            u32_le(bytes)
        }
    };
    let _real64 = take(4)?;
    let _major = take(4)?;
    let _minor = take(4)?;
    let _format = take(4)?;
    let class_len = u32_of(take(4)?) as usize;
    let _class = take(class_len)?;
    let _import_offset = take(8)?;
    let flags = u32_of(take(4)?);
    if flags & FORMAT_FLAG_UIDS == 0 {
        return Some(BinaryUid::Missing);
    }
    let raw = take(8)?;
    let mut bytes = [0_u8; 8];
    bytes.copy_from_slice(raw);
    let id = if big_endian {
        i64::from_be_bytes(bytes)
    } else {
        i64::from_le_bytes(bytes)
    };
    Some(if id < 0 {
        BinaryUid::Missing
    } else {
        BinaryUid::Uid(uid_text(id as u64))
    })
}

fn u32_le(bytes: &[u8]) -> u32 {
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

/// A uid id as Godot's `ResourceUID::id_to_text` writes it.
pub(super) fn uid_text(mut id: u64) -> String {
    const DIGITS: &[u8; 34] = b"abcdefghijklmnopqrstuvwxy012345678";
    let mut digits = Vec::new();
    loop {
        digits.push(DIGITS[(id % 34) as usize]);
        id /= 34;
        if id == 0 {
            break;
        }
    }
    digits.reverse();
    format!("uid://{}", String::from_utf8_lossy(&digits))
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    /// The header Godot 4.8 writes for a packed scene, with `uid` (or none).
    pub fn rsrc(uid: Option<i64>) -> Vec<u8> {
        let mut bytes = b"RSRC".to_vec();
        for word in [0_u32, 0, 4, 8, 6] {
            bytes.extend(word.to_le_bytes());
        }
        let class = b"PackedScene\0";
        bytes.extend((class.len() as u32).to_le_bytes());
        bytes.extend(class);
        bytes.extend(0_u64.to_le_bytes());
        bytes.extend(3_u32.to_le_bytes());
        bytes.extend(uid.unwrap_or(-1).to_le_bytes());
        bytes.extend([0_u8; 64]);
        bytes
    }

    /// The file `rsrc` bytes become when saved compressed: `RSCC`, then the rest of the
    /// stream (everything after `RSRC`) in zstd blocks of 4 KiB, as `FileAccessCompressed` writes.
    pub fn rscc(rsrc: &[u8]) -> Vec<u8> {
        let body = &rsrc[4..];
        let blocks: Vec<Vec<u8>> = body
            .chunks(4096)
            .map(|chunk| {
                let mut out = Vec::new();
                ruzstd::encoding::compress(
                    chunk,
                    &mut out,
                    ruzstd::encoding::CompressionLevel::Fastest,
                );
                out
            })
            .collect();
        let mut bytes = b"RSCC".to_vec();
        bytes.extend(MODE_ZSTD.to_le_bytes());
        bytes.extend(4096_u32.to_le_bytes());
        bytes.extend((body.len() as u32).to_le_bytes());
        for block in &blocks {
            bytes.extend((block.len() as u32).to_le_bytes());
        }
        for block in &blocks {
            bytes.extend(block);
        }
        bytes
    }

    #[test]
    fn the_uid_is_read_from_plain_and_compressed_headers() {
        let dir = tempfile::tempdir().expect("dir");
        let id = 0x63cf_e4ba_4e3a_8225_i64;
        let cases: [(&str, Vec<u8>, BinaryUid); 5] = [
            (
                "plain.scn",
                rsrc(Some(id)),
                BinaryUid::Uid(uid_text(id as u64)),
            ),
            ("none.scn", rsrc(None), BinaryUid::Missing),
            (
                "packed.scn",
                rscc(&rsrc(Some(id))),
                BinaryUid::Uid(uid_text(id as u64)),
            ),
            (
                "large.res",
                rscc(&[rsrc(Some(7)), vec![9_u8; 9000]].concat()),
                BinaryUid::Uid(uid_text(7)),
            ),
            (
                "text.res",
                b"[gd_resource]\n".to_vec(),
                BinaryUid::Unreadable,
            ),
        ];
        for (name, bytes, expected) in cases {
            let file = dir.path().join(name);
            std::fs::write(&file, bytes).expect("write");
            assert_eq!(binary_uid(&file), expected, "{name}");
        }
        // The bytes ResourceSaver.set_uid wrote for this uid in Godot 4.8.
        assert_eq!(uid_text(id as u64), "uid://dap714cpjw8pd");
    }

    #[test]
    fn a_damaged_compressed_header_is_unreadable_and_allocates_nothing() {
        let dir = tempfile::tempdir().expect("dir");
        // One-byte blocks and a size of 4 GiB: the table of block sizes it promises cannot exist.
        let mut bytes = b"RSCC".to_vec();
        for word in [MODE_ZSTD, 1, u32::MAX, 16] {
            bytes.extend(word.to_le_bytes());
        }
        bytes.extend([0_u8; 32]);
        let file = dir.path().join("damaged.scn");
        std::fs::write(&file, bytes).expect("write");
        assert_eq!(binary_uid(&file), BinaryUid::Unreadable);
    }
}
