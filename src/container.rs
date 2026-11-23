//! The container both file formats share: LZ4 blocks in K4os pickler frames.
//!
//! A `.bin.lz4` file is:
//!
//! ```text
//! int32            size hint of the writer's buffer (not needed to read)
//! repeat until the end of the file:
//!     int32        N, the length of the frame that follows
//!     N bytes      one frame, holding one block of records
//! ```
//!
//! A frame starts with one header byte: bits 0-2 are the frame version
//! (always 0) and bits 6-7 the width of the `diff` field that follows (0, 1,
//! 2 or 4 bytes). `diff` is how many bytes compression saved; 0 means the rest
//! of the frame is the block as is, otherwise it is one raw LZ4 block that
//! decompresses to `payload length + diff` bytes. This is the format of the
//! `K4os.Compression.LZ4` pickler. It is not the LZ4 *frame* format, so the
//! `lz4` command line tool cannot read these files.
//!
//! A decompressed block is a sequence of records. Every record starts with
//! the same 12-byte header: `int16 kind`, `uint16 size` (of the whole record,
//! header included) and `int64 time`, all little-endian.
//!
//! [`Blocks`] reads the blocks of a file, [`RawRecords`] the records with
//! their kind and time; [`crate::l2::Reader`] and [`crate::orderlog::Reader`]
//! decode them. Memory stays small whatever the size of the file: one frame
//! is read at a time.
//!
//! See <https://docs.alphaquick.io/data/l2-depth-trades#container>.

use std::fs::File;
use std::io::{self, BufReader, Read};
use std::path::Path;

use crate::error::{Error, Result};

/// Length of the header every record starts with.
pub const HEADER_LEN: usize = 12;

/// The largest frame, and the largest decompressed block, this reader
/// accepts (1 GiB). The writers produce far smaller ones; a larger length
/// means the file is not a capture or is damaged.
pub const MAX_FRAME_LEN: usize = 1 << 30;

const DIFF_WIDTHS: [usize; 4] = [0, 1, 2, 4];

/// Decode one frame into the block of records it holds.
pub fn decode_frame(frame: &[u8]) -> Result<Vec<u8>> {
    let mut block = Vec::new();
    decode_frame_into(frame, &mut block)?;
    Ok(block)
}

/// Decode one frame into `block`, reusing its allocation. `block` is
/// cleared first; on an error it is left empty.
pub fn decode_frame_into(frame: &[u8], block: &mut Vec<u8>) -> Result<()> {
    block.clear();
    let (&header, rest) = frame
        .split_first()
        .ok_or_else(|| Error::format("empty frame"))?;
    if header & 0x07 != 0 {
        return Err(Error::format(format!(
            "unsupported frame version {}",
            header & 0x07
        )));
    }
    let width = DIFF_WIDTHS[usize::from(header >> 6)];
    if rest.len() < width {
        return Err(Error::format("frame shorter than its header"));
    }
    let mut diff = [0u8; 4];
    diff[..width].copy_from_slice(&rest[..width]);
    let diff = u32::from_le_bytes(diff) as usize;
    let payload = &rest[width..];
    if diff == 0 {
        block.extend_from_slice(payload);
        return Ok(());
    }
    // LZ4 expands at most 255 times (plus a few bytes): refuse a larger
    // claim before allocating for it.
    let most = payload.len().saturating_mul(255).saturating_add(16);
    if diff > most {
        return Err(Error::format(format!(
            "frame says it decompresses to {} bytes, more than LZ4 can expand {} bytes to",
            payload.len().saturating_add(diff),
            payload.len()
        )));
    }
    let expected = payload
        .len()
        .checked_add(diff)
        .filter(|&n| n <= MAX_FRAME_LEN)
        .ok_or_else(|| {
            Error::format(format!(
                "frame says it decompresses to more than {MAX_FRAME_LEN} bytes"
            ))
        })?;
    block.resize(expected, 0);
    match lz4_flex::block::decompress_into(payload, block) {
        Ok(n) if n == expected => Ok(()),
        Ok(n) => {
            block.clear();
            Err(Error::format(format!(
                "corrupt LZ4 block: {n} bytes decompressed, {expected} expected"
            )))
        }
        Err(error) => {
            block.clear();
            Err(Error::format(format!("corrupt LZ4 block: {error}")))
        }
    }
}

/// Read into `buf` until it is full or the source ends; the number of bytes read.
fn read_full<R: Read>(reader: &mut R, buf: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match reader.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(filled)
}

/// The decompressed blocks of a file, in order.
///
/// [`Blocks::next_block`] lends each block without copying it; the
/// [`Iterator`] implementation yields owned copies. An empty file holds no
/// blocks. After an error the reader is finished.
///
/// ```no_run
/// use alphaquick::container::Blocks;
///
/// let mut blocks = Blocks::open("BTC_HYPERLIQUID_20261101.bin.lz4")?;
/// let mut bytes = 0;
/// while let Some(block) = blocks.next_block()? {
///     bytes += block.len();
/// }
/// println!("{bytes} bytes of records");
/// # Ok::<(), alphaquick::Error>(())
/// ```
#[derive(Debug)]
pub struct Blocks<R> {
    reader: R,
    started: bool,
    finished: bool,
    frame: Vec<u8>,
    pub(crate) block: Vec<u8>,
}

impl Blocks<BufReader<File>> {
    /// Open a file.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Ok(Blocks::new(BufReader::with_capacity(
            1 << 16,
            File::open(path)?,
        )))
    }
}

impl<R: Read> Blocks<R> {
    /// Read the blocks from any byte source: a file, a `&[u8]`, a network stream.
    pub fn new(reader: R) -> Self {
        Blocks {
            reader,
            started: false,
            finished: false,
            frame: Vec::new(),
            block: Vec::new(),
        }
    }

    /// The next block, borrowed until the next call; `None` at the end of the file.
    pub fn next_block(&mut self) -> Result<Option<&[u8]>> {
        if self.finished {
            return Ok(None);
        }
        match self.read_next() {
            Ok(true) => Ok(Some(&self.block)),
            Ok(false) => {
                self.finished = true;
                Ok(None)
            }
            Err(error) => {
                self.finished = true;
                self.block.clear();
                Err(error)
            }
        }
    }

    /// The source the blocks are read from.
    pub fn into_inner(self) -> R {
        self.reader
    }

    fn read_next(&mut self) -> Result<bool> {
        if !self.started {
            self.started = true;
            let mut hint = [0u8; 4];
            match read_full(&mut self.reader, &mut hint)? {
                0 => return Ok(false),
                4 => {}
                _ => {
                    return Err(Error::truncated(
                        "the file is shorter than its 4-byte header",
                    ))
                }
            }
        }
        let mut head = [0u8; 4];
        match read_full(&mut self.reader, &mut head)? {
            0 => return Ok(false),
            4 => {}
            _ => return Err(Error::truncated("the file ends inside a frame length")),
        }
        let length = i32::from_le_bytes(head);
        if length <= 0 || length as usize > MAX_FRAME_LEN {
            return Err(Error::format(format!("invalid frame length {length}")));
        }
        let length = length as usize;
        self.frame.clear();
        self.frame.reserve(length.min(1 << 20));
        (&mut self.reader)
            .take(length as u64)
            .read_to_end(&mut self.frame)?;
        if self.frame.len() < length {
            return Err(Error::truncated(format!(
                "the file ends inside a frame ({} of {length} bytes)",
                self.frame.len()
            )));
        }
        decode_frame_into(&self.frame, &mut self.block)?;
        Ok(true)
    }
}

impl<R: Read> Iterator for Blocks<R> {
    type Item = Result<Vec<u8>>;

    fn next(&mut self) -> Option<Self::Item> {
        self.next_block().map(|b| b.map(<[u8]>::to_vec)).transpose()
    }
}

/// One record as it is stored: its kind, its time and all its bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RawRecord<'a> {
    /// The record kind (L2: 0 depth update, 1 trade; order log: 16 to 22).
    pub kind: i16,
    /// The time in the header: 100 ns ticks since 0001-01-01 UTC.
    pub time: i64,
    /// The whole record, header included: `bytes.len()` is its `size`.
    pub bytes: &'a [u8],
}

impl<'a> RawRecord<'a> {
    /// The record after its 12-byte header.
    pub fn body(&self) -> &'a [u8] {
        &self.bytes[HEADER_LEN..]
    }
}

fn record_at(block: &[u8], pos: usize) -> Result<RawRecord<'_>> {
    let rest = block.len() - pos;
    if rest < HEADER_LEN {
        return Err(Error::format(format!(
            "a record header is cut off at the end of a block ({rest} bytes left)"
        )));
    }
    let kind = i16::from_le_bytes([block[pos], block[pos + 1]]);
    let size = usize::from(u16::from_le_bytes([block[pos + 2], block[pos + 3]]));
    if size < HEADER_LEN || size > rest {
        return Err(Error::format(format!(
            "record of kind {kind} at offset {pos} has an invalid size {size}"
        )));
    }
    let mut time = [0u8; 8];
    time.copy_from_slice(&block[pos + 4..pos + 12]);
    Ok(RawRecord {
        kind,
        time: i64::from_le_bytes(time),
        bytes: &block[pos..pos + size],
    })
}

/// The records of one decompressed block; see [`records_of_block`].
#[derive(Clone, Debug)]
pub struct BlockRecords<'a> {
    block: &'a [u8],
    pos: usize,
    failed: bool,
}

/// Iterate over the records of one decompressed block.
///
/// A record whose size does not fit the block is a [`Error::Format`], after
/// which the iterator ends.
pub fn records_of_block(block: &[u8]) -> BlockRecords<'_> {
    BlockRecords {
        block,
        pos: 0,
        failed: false,
    }
}

impl<'a> Iterator for BlockRecords<'a> {
    type Item = Result<RawRecord<'a>>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed || self.pos >= self.block.len() {
            return None;
        }
        match record_at(self.block, self.pos) {
            Ok(record) => {
                self.pos += record.bytes.len();
                Some(Ok(record))
            }
            Err(error) => {
                self.failed = true;
                Some(Err(error))
            }
        }
    }
}

/// Every record of a file, in file order, undecoded.
///
/// This is the lowest level of the crate: [`crate::l2::Reader`] and
/// [`crate::orderlog::Reader`] build on it. Records are lent without copying,
/// so this is not an [`Iterator`]; loop with [`RawRecords::next_record`].
///
/// ```no_run
/// use alphaquick::container::RawRecords;
///
/// let mut records = RawRecords::open("xyz_CL_HLC_20261101.bin.lz4")?;
/// let mut counts = std::collections::BTreeMap::new();
/// while let Some(record) = records.next_record()? {
///     *counts.entry(record.kind).or_insert(0u64) += 1;
/// }
/// println!("{counts:?}");
/// # Ok::<(), alphaquick::Error>(())
/// ```
#[derive(Debug)]
pub struct RawRecords<R> {
    blocks: Blocks<R>,
    pos: usize,
    loaded: bool,
    failed: bool,
}

impl RawRecords<BufReader<File>> {
    /// Open a file.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Ok(RawRecords::from_blocks(Blocks::open(path)?))
    }
}

impl<R: Read> RawRecords<R> {
    /// Read the records from any byte source.
    pub fn new(reader: R) -> Self {
        RawRecords::from_blocks(Blocks::new(reader))
    }

    /// Read the records of these blocks.
    pub fn from_blocks(blocks: Blocks<R>) -> Self {
        RawRecords {
            blocks,
            pos: 0,
            loaded: false,
            failed: false,
        }
    }

    /// The next record, borrowed until the next call; `None` at the end of
    /// the file. After an error the reader is finished.
    pub fn next_record(&mut self) -> Result<Option<RawRecord<'_>>> {
        if self.failed {
            return Ok(None);
        }
        while !self.loaded || self.pos >= self.blocks.block.len() {
            match self.blocks.next_block() {
                Ok(Some(_)) => {
                    self.loaded = true;
                    self.pos = 0;
                }
                Ok(None) => return Ok(None),
                Err(error) => {
                    self.failed = true;
                    return Err(error);
                }
            }
        }
        match record_at(&self.blocks.block, self.pos) {
            Ok(record) => {
                self.pos += record.bytes.len();
                Ok(Some(record))
            }
            Err(error) => {
                self.failed = true;
                Err(error)
            }
        }
    }
}

// Little-endian field readers for the record decoders. The callers check the
// record length first, so the slices are always in bounds.
pub(crate) fn u16_at(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

pub(crate) fn u32_at(b: &[u8], at: usize) -> u32 {
    let mut a = [0u8; 4];
    a.copy_from_slice(&b[at..at + 4]);
    u32::from_le_bytes(a)
}

pub(crate) fn u64_at(b: &[u8], at: usize) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[at..at + 8]);
    u64::from_le_bytes(a)
}

pub(crate) fn i64_at(b: &[u8], at: usize) -> i64 {
    u64_at(b, at) as i64
}

pub(crate) fn need(record: &RawRecord<'_>, minimum: usize) -> Result<()> {
    if record.bytes.len() < minimum {
        return Err(Error::format(format!(
            "record of kind {} has {} bytes, fewer than its {minimum}",
            record.kind,
            record.bytes.len()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;

    #[test]
    fn stored_and_compressed_frames_decode_to_the_same_block() {
        for size in [300usize, 70_000, 400_000] {
            let block: Vec<u8> = b"abcdefgh".iter().copied().cycle().take(size).collect();
            assert_eq!(decode_frame(&frame(&block, false)).unwrap(), block);
            let packed = frame(&block, true);
            assert!(matches!(packed[0] >> 6, 1..=3));
            assert_eq!(decode_frame(&packed).unwrap(), block);
        }
        let widths: std::collections::BTreeSet<u8> = [100usize, 30_000, 400_000]
            .iter()
            .map(|&n| frame(&vec![b'x'; n], true)[0] >> 6)
            .collect();
        assert_eq!(widths.into_iter().collect::<Vec<_>>(), vec![1, 2, 3]);
    }

    #[test]
    fn blocks_come_from_files_and_byte_slices() {
        let blocks = vec![depth(1, 100.0, 1.0, 1).repeat(50), depth(2, 101.0, 2.0, 2)];
        let data = container(&blocks, true);
        let read: Vec<Vec<u8>> = Blocks::new(&data[..]).map(|b| b.unwrap()).collect();
        assert_eq!(read, blocks);
        let dir = std::env::temp_dir().join(format!("alphaquick-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("x.bin.lz4");
        std::fs::write(&path, &data).unwrap();
        let read: Vec<Vec<u8>> = Blocks::open(&path).unwrap().map(|b| b.unwrap()).collect();
        assert_eq!(read, blocks);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn all_blocks(data: &[u8]) -> Result<Vec<Vec<u8>>> {
        Blocks::new(data).collect()
    }

    #[test]
    fn empty_and_truncated_files() {
        assert!(all_blocks(b"").unwrap().is_empty());
        assert!(all_blocks(&1i32.to_le_bytes()).unwrap().is_empty());
        assert!(matches!(all_blocks(b"\x00\x00"), Err(Error::Truncated(_))));
        let data = container(&[depth(1, 1.0, 1.0, 1).repeat(100)], true);
        for cut in [6, data.len() - 1] {
            assert!(
                matches!(all_blocks(&data[..cut]), Err(Error::Truncated(_))),
                "{cut}"
            );
        }
        // An error ends the iteration.
        let mut blocks = Blocks::new(&data[..data.len() - 1]);
        assert!(blocks.next().unwrap().is_err());
        assert!(blocks.next().is_none());
    }

    fn format_message(result: Result<Vec<u8>>) -> String {
        match result {
            Err(Error::Format(message)) => message,
            other => panic!("expected a format error, got {other:?}"),
        }
    }

    #[test]
    fn malformed_frames_are_refused() {
        assert!(format_message(decode_frame(b"\x01abc")).contains("version"));
        assert!(format_message(decode_frame(b"")).contains("empty"));
        let zero: Vec<u8> = [1i32.to_le_bytes(), 0i32.to_le_bytes()].concat();
        assert!(
            matches!(all_blocks(&zero), Err(Error::Format(m)) if m.contains("invalid frame length"))
        );
        let negative: Vec<u8> = [1i32.to_le_bytes(), (-5i32).to_le_bytes()].concat();
        assert!(matches!(all_blocks(&negative), Err(Error::Format(_))));
        assert!(
            format_message(decode_frame(&[1 << 6, 200, 0xff, 0xff, 0xff, 0xff]))
                .contains("corrupt LZ4")
        );
        assert!(format_message(decode_frame(&[3 << 6, 1])).contains("shorter than its header"));
        // A valid LZ4 block whose size does not match diff.
        let block = vec![b'y'; 1000];
        let mut packed = frame(&block, true);
        packed[1] = packed[1].wrapping_add(1);
        assert!(format_message(decode_frame(&packed)).contains("corrupt LZ4"));
    }

    #[test]
    fn a_frame_claiming_more_than_lz4_can_expand_is_refused_before_allocating() {
        // 14 bytes: header, one frame of a 4-byte diff (~750 MB) and 4 bytes of payload.
        let mut file = 65536i32.to_le_bytes().to_vec();
        file.extend_from_slice(&6i32.to_le_bytes());
        file.extend_from_slice(&[3 << 6, 0x00, 0x00, 0xb5, 0x2c]);
        file.push(0x10);
        assert_eq!(file.len(), 14);
        let error = all_blocks(&file).unwrap_err();
        assert!(
            matches!(&error, Error::Format(m) if m.contains("more than LZ4 can expand")),
            "{error}"
        );
        // A real frame at the limit still decodes: 255 times expansion is possible.
        let block = vec![0u8; 60_000];
        assert_eq!(decode_frame(&frame(&block, true)).unwrap(), block);
    }

    #[test]
    fn raw_records_and_their_framing() {
        let block = [depth(5, 1.0, 2.0, 1), unknown(99, 6), depth(7, 1.5, 1.0, 2)].concat();
        let data = container(&[block.clone()], true);
        let mut records = RawRecords::new(&data[..]);
        let mut seen = Vec::new();
        while let Some(record) = records.next_record().unwrap() {
            seen.push((record.kind, record.bytes.len(), record.time));
        }
        assert_eq!(seen, vec![(0, 29, 5), (99, 15, 6), (0, 29, 7)]);
        let of_block: Vec<_> = records_of_block(&block).map(|r| r.unwrap().kind).collect();
        assert_eq!(of_block, vec![0, 99, 0]);

        let bad = [header(0, 200, 1), vec![0; 10]].concat();
        let message = RawRecords::new(&container(&[bad], false)[..]).into_err();
        assert!(message.contains("invalid size"));
        let cut = [depth(1, 1.0, 1.0, 1), vec![0, 0, 0]].concat();
        let message = RawRecords::new(&container(&[cut], false)[..]).into_err();
        assert!(message.contains("cut off"));
    }

    trait IntoErr {
        fn into_err(self) -> String;
    }

    impl<R: Read> IntoErr for RawRecords<R> {
        fn into_err(mut self) -> String {
            loop {
                match self.next_record() {
                    Ok(Some(_)) => {}
                    Ok(None) => panic!("no error"),
                    Err(Error::Format(message)) => {
                        assert!(self.next_record().unwrap().is_none());
                        return message;
                    }
                    Err(other) => panic!("{other}"),
                }
            }
        }
    }
}
