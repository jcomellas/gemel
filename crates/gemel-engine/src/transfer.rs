//! Sending file contents: as they are, or as rsync deltas against the
//! destination's old version.
//!
//! Ports Unison `src/transfer.ml`, Unison `src/checksum.ml`.
//!
//! Derived from Unison, Copyright 1999-2020, Benjamin C. Pierce.
//!
//! The source sends tokens, which the destination decodes as they come:
//! - `S`, a 2-byte length, then that many bytes of contents;
//! - `B`, a 3-byte block number, then a 1-byte count: that many blocks of
//!   the destination's old file, from that one on;
//! - `E`, the end of the file.
//!
//! Tokens are gathered in a queue of 65 500 bytes, which is sent when the
//! next token wouldn't fit, and at the end ([`TokenQueue`]). Consecutive
//! strings merge into one, and so do blocks that follow each other, up to
//! 255. A token never spans two chunks. OCaml decodes each chunk on its
//! own, so the chunks, not only their concatenation, are OCaml's.
//!
//! For rsync ([`rsync`]), the destination first cuts its old file into
//! blocks and sends a weak (rolling) checksum and the start of an MD5 for
//! each ([`rsync::BlockInfo`]). The source slides a block-sized window
//! over the new file: where the window's checksums match a block, it sends
//! that block's number; elsewhere, the bytes. The block size and the length
//! of the strong checksums depend on the files' lengths
//! ([`rsync::sizes`]).
//!
//! Everything here is synchronous, for blocking tasks: the files are a
//! [`Read`] (and [`Seek`]) and a [`Write`], and chunks go out, and progress
//! is reported, through callbacks, which can stop the transfer with an
//! error.

use std::io::{Read, Seek, SeekFrom, Write};

use gemel_base::util::Error;
use gemel_fs::props::Filesize;

/// The rolling checksum (`Checksum`): over bytes `c_n ... c_0`, the sum of
/// `c_i * 16381^i`, on 31 bits.
pub mod checksum {
    /// `Checksum.init`: the table for rolling a window of `l` bytes,
    /// `i * 16381^(l + 1)` for each byte value `i`.
    pub fn init(l: usize) -> [u64; 256] {
        // OCaml's 63-bit ints wrap as these do in their low 31 bits, the
        // only ones kept.
        let mut p: u64 = 1;
        for _ in 0..=l {
            p = p.wrapping_mul(16381);
        }
        std::array::from_fn(|i| (i as u64).wrapping_mul(p) & 0x7fff_ffff)
    }

    /// `Checksum.roll`: the checksum of the window moved one byte on,
    /// `outgoing` leaving it and `incoming` entering it.
    pub fn roll(table: &[u64; 256], cksum: u32, outgoing: u8, incoming: u8) -> u32 {
        let v = u64::from(cksum) + u64::from(incoming);
        ((v << 14)
            .wrapping_sub(v + v + v)
            .wrapping_sub(table[usize::from(outgoing)])
            & 0x7fff_ffff) as u32
    }

    /// `Checksum.subbytes`: the checksum of `s`.
    pub fn of(s: &[u8]) -> u32 {
        let mut cksum: u64 = 0;
        for &c in s {
            let v = cksum.wrapping_add(u64::from(c));
            cksum = (v << 14).wrapping_sub(v.wrapping_mul(3));
        }
        (cksum & 0x7fff_ffff) as u32
    }
}

/// Reports progress (bytes sent or received); an error stops the transfer
/// (an abort).
pub type Progress<'a> = dyn FnMut(usize) -> Result<(), Error> + 'a;
/// Sends a chunk of tokens.
pub type Transmit<'a> = dyn FnMut(&[u8]) -> Result<(), Error> + 'a;

/// The size of the token queue (`queueSize`): a string token's length
/// fits in 2 bytes.
pub const QUEUE_SIZE: usize = 65500;

/// What the last token queued was, for merging the next one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Previous {
    None,
    /// A string of this length.
    Str(usize),
    /// Blocks, the next of which would be this one.
    Block(usize),
}

/// The token queue (`tokenQueue`).
struct TokenQueue {
    data: Vec<u8>,
    previous: Previous,
    /// The size of the contents the queued tokens stand for.
    prog: usize,
    block_size: usize,
}

impl TokenQueue {
    /// `makeQueue`.
    fn new(block_size: usize) -> TokenQueue {
        TokenQueue {
            data: Vec::with_capacity(QUEUE_SIZE),
            previous: Previous::None,
            prog: 0,
            block_size,
        }
    }

    /// `flushQueue`: sends the queue if `cond` and it isn't empty.
    fn flush(
        &mut self,
        cond: bool,
        progress: &mut Progress<'_>,
        transmit: &mut Transmit<'_>,
    ) -> Result<(), Error> {
        if cond && !self.data.is_empty() {
            transmit(&self.data)?;
            progress(self.prog)?;
            self.data.clear();
            self.prog = 0;
            self.previous = Previous::None;
        }
        Ok(())
    }

    /// `pushEOF`.
    fn push_eof(
        &mut self,
        progress: &mut Progress<'_>,
        transmit: &mut Transmit<'_>,
    ) -> Result<(), Error> {
        self.flush(self.data.len() + 1 > QUEUE_SIZE, progress, transmit)?;
        self.data.push(b'E');
        self.previous = Previous::None;
        Ok(())
    }

    /// `pushString`.
    fn push_string(
        &mut self,
        mut s: &[u8],
        progress: &mut Progress<'_>,
        transmit: &mut Transmit<'_>,
    ) -> Result<(), Error> {
        loop {
            self.flush(
                self.data.len() + s.len() + 3 > QUEUE_SIZE,
                progress,
                transmit,
            )?;
            let l = s.len().min(QUEUE_SIZE - self.data.len() - 3);
            self.data.push(b'S');
            self.data.extend_from_slice(&(l as u16).to_le_bytes());
            self.data.extend_from_slice(&s[..l]);
            self.prog += l;
            self.previous = Previous::Str(l);
            s = &s[l..];
            if s.is_empty() {
                return Ok(());
            }
        }
    }

    /// `growString`: adds to the string token last queued, as far as the
    /// queue goes, then starts another.
    fn grow_string(
        &mut self,
        len: usize,
        s: &[u8],
        progress: &mut Progress<'_>,
        transmit: &mut Transmit<'_>,
    ) -> Result<(), Error> {
        let l = (QUEUE_SIZE - self.data.len()).min(s.len());
        let head = self.data.len() - len - 2;
        self.data.extend_from_slice(&s[..l]);
        let grown = len + l;
        self.data[head..head + 2].copy_from_slice(&(grown as u16).to_le_bytes());
        self.prog += l;
        self.previous = Previous::Str(grown);
        if l < s.len() {
            self.push_string(&s[l..], progress, transmit)
        } else {
            Ok(())
        }
    }

    /// `pushBlock`.
    fn push_block(
        &mut self,
        n: usize,
        progress: &mut Progress<'_>,
        transmit: &mut Transmit<'_>,
    ) -> Result<(), Error> {
        self.flush(self.data.len() + 5 > QUEUE_SIZE, progress, transmit)?;
        self.data.push(b'B');
        self.data.extend_from_slice(&(n as u32).to_le_bytes()[..3]);
        self.data.push(1);
        self.prog += self.block_size;
        self.previous = Previous::Block(n + 1);
        Ok(())
    }

    /// `growBlock`: one more block on the block token last queued.
    fn grow_block(&mut self, n: usize) {
        let last = self.data.len() - 1;
        let count = self.data[last];
        self.data[last] = count + 1;
        self.prog += self.block_size;
        self.previous = if count == 254 {
            Previous::None
        } else {
            Previous::Block(n + 1)
        };
    }

    /// `queueToken` for a string.
    fn string(
        &mut self,
        s: &[u8],
        progress: &mut Progress<'_>,
        transmit: &mut Transmit<'_>,
    ) -> Result<(), Error> {
        match self.previous {
            Previous::Str(len) => self.grow_string(len, s, progress, transmit),
            _ => self.push_string(s, progress, transmit),
        }
    }

    /// `queueToken` for a block.
    fn block(
        &mut self,
        n: usize,
        progress: &mut Progress<'_>,
        transmit: &mut Transmit<'_>,
    ) -> Result<(), Error> {
        if self.previous == Previous::Block(n) {
            self.grow_block(n);
            Ok(())
        } else {
            self.push_block(n, progress, transmit)
        }
    }
}

/// `reallyRead`: reads into `buf` until it is full or the input ends, and
/// returns how much was read.
fn really_read(input: &mut dyn Read, buf: &mut [u8]) -> Result<usize, Error> {
    let mut n = 0;
    while n < buf.len() {
        match input.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(io_error("reading", &e)),
        }
    }
    Ok(n)
}

fn io_error(what: &str, e: &std::io::Error) -> Error {
    Error::Transient(format!("Error {what} a file: {e}"))
}

fn corrupt(what: &str) -> Error {
    Error::Transient(format!(
        "Internal error during rsync transfer ({what}): unexpected data"
    ))
}

/// `Transfer.send`: sends `length` bytes of `input` as plain contents (or
/// fewer, if it ends first), then the end.
pub fn send(
    input: &mut dyn Read,
    length: Filesize,
    progress: &mut Progress<'_>,
    transmit: &mut Transmit<'_>,
) -> Result<(), Error> {
    const BUF_SIZE: usize = 8192;
    let mut buf = [0; BUF_SIZE];
    let mut q = TokenQueue::new(0);
    let mut length = length;
    while length > 0 {
        let want = if length > BUF_SIZE as Filesize {
            BUF_SIZE
        } else {
            length as usize
        };
        let count = really_read(input, &mut buf[..want])?;
        if count == 0 {
            break;
        }
        q.string(&buf[..count], progress, transmit)?;
        length -= count as Filesize;
    }
    q.push_eof(progress, transmit)?;
    q.flush(true, progress, transmit)
}

/// `Transfer.receive`: writes a chunk of plain contents; true at the end.
pub fn receive(
    output: &mut dyn Write,
    progress: &mut Progress<'_>,
    chunk: &[u8],
) -> Result<bool, Error> {
    let mut pos = 0;
    while pos < chunk.len() {
        match chunk[pos] {
            b'S' => {
                let s = string_token(chunk, pos).ok_or_else(|| corrupt("receiving"))?;
                output.write_all(s).map_err(|e| io_error("writing", &e))?;
                progress(s.len())?;
                pos += s.len() + 3;
            }
            b'E' => return Ok(true),
            _ => return Err(corrupt("receiving")),
        }
    }
    Ok(false)
}

/// The contents of the string token at `pos`.
fn string_token(chunk: &[u8], pos: usize) -> Option<&[u8]> {
    let len = chunk.get(pos + 1..pos + 3)?;
    let len = usize::from(u16::from_le_bytes([len[0], len[1]]));
    chunk.get(pos + 3..pos + 3 + len)
}

/// The rsync algorithm (`Transfer.Rsync`).
pub mod rsync {
    use super::*;
    use gemel_compat::umarshal::{self, Reader, Writer};
    use md5::{Digest, Md5};

    /// Blocks are at least this big (`minBlockSize`), and files no bigger
    /// go as they are.
    pub const MIN_BLOCK_SIZE: usize = 700;

    /// `aboveRsyncThreshold`: whether a file is big enough for rsync.
    pub fn above_rsync_threshold(size: Filesize) -> bool {
        size > MIN_BLOCK_SIZE as Filesize
    }

    /// `logProba`: the worst-case probability of a failure, one in 2^27.
    const LOG_PROBA: f64 = -27.;
    /// `weakLen`: how many bits of the weak checksum count.
    const WEAK_LEN: f64 = 27.;

    /// `computeBlockSize`: about the square root of the old file's length,
    /// rounded to a power of 2 between 2^10 and 2^17.
    fn block_size(l: f64) -> usize {
        let bits = (l.sqrt().ln() / 2f64.ln()).round().clamp(10., 17.);
        1 << (bits as u32)
    }

    /// `checksumSize`: how many bytes of each block's MD5 are sent, from 2
    /// to 16.
    fn checksum_size(bs: usize, sl: f64, dl: f64) -> usize {
        let bits = -LOG_PROBA - WEAK_LEN + (sl * dl / bs as f64).ln() / 2f64.ln();
        (((bits + 7.99) / 8.) as i64).clamp(2, 16) as usize
    }

    /// `sizes`: the block size, the number of blocks (at most 2^24) and the
    /// strong checksums' size, for these source and destination lengths.
    pub fn sizes(src_length: Filesize, dst_length: Filesize) -> (usize, usize, usize) {
        let bs = block_size(dst_length as f64);
        let count = (dst_length / bs as Filesize).min(16_777_216) as usize;
        let cs = checksum_size(bs, src_length as f64, dst_length as f64);
        (bs, count, cs)
    }

    /// `memoryFootprint`: the size of the block information.
    pub fn memory_footprint(src_length: Filesize, dst_length: Filesize) -> usize {
        let (_, count, cs) = sizes(src_length, dst_length);
        count * (cs + 4)
    }

    /// What the destination sends of its old file (`rsync_block_info`).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct BlockInfo {
        /// The block size.
        pub block_size: usize,
        /// How many blocks there are.
        pub block_count: usize,
        /// The size of each strong checksum.
        pub checksum_size: usize,
        /// Each block's weak checksum (as many as [`sizes`] said, which
        /// can be more than the blocks read).
        pub weak: Vec<i32>,
        /// Each block's strong checksum, one after the other.
        pub strong: Vec<u8>,
    }

    impl BlockInfo {
        /// `mrsync_block_info`.
        pub fn marshal(&self, w: &mut Writer) {
            w.int(self.block_size as i64);
            w.int(self.block_count as i64);
            w.int(self.checksum_size as i64);
            w.int32bigarray(&self.weak);
            w.bytearray(&self.strong);
        }

        /// `mrsync_block_info`.
        pub fn unmarshal(r: &mut Reader<'_>) -> umarshal::Result<BlockInfo> {
            let size = |n: i64| usize::try_from(n).map_err(|_| umarshal::Error::NegativeLength(n));
            Ok(BlockInfo {
                block_size: size(r.int()?)?,
                block_count: size(r.int()?)?,
                checksum_size: size(r.int()?)?,
                weak: r.int32bigarray()?,
                strong: r.bytearray()?,
            })
        }

        /// Whether the counts agree with the checksums (OCaml checks it
        /// before using the information).
        fn check(&self, what: &str) -> Result<(), Error> {
            if self.block_count > self.weak.len()
                || self.block_count * self.checksum_size > self.strong.len()
                || self.checksum_size > 16
                || self.block_size == 0
            {
                return Err(Error::Transient(format!(
                    "Internal error during rsync transfer ({what}), please report: {} {} - {} {}",
                    self.block_count,
                    self.weak.len(),
                    self.block_count * self.checksum_size,
                    self.strong.len()
                )));
            }
            Ok(())
        }
    }

    fn md5(block: &[u8]) -> [u8; 16] {
        Md5::digest(block).into()
    }

    /// `rsyncPreprocess`: cuts the destination's old file into blocks (from
    /// its start) and computes their checksums, for files of these lengths.
    pub fn preprocess<R: Read + Seek>(
        input: &mut R,
        src_length: Filesize,
        dst_length: Filesize,
    ) -> Result<BlockInfo, Error> {
        let (bs, count, cs) = sizes(src_length, dst_length);
        let mut weak = vec![0; count];
        let mut strong = vec![0; count * cs];
        input
            .seek(SeekFrom::Start(0))
            .map_err(|e| io_error("reading", &e))?;
        // `blockIter`, as far as the blocks fit in `encodeInt3`.
        let max = count.min(1 << 24);
        let mut buf = vec![0; 8192 + bs];
        let (mut n, mut offset, mut length) = (0, 0, 0);
        while n < max {
            if offset + bs <= length {
                let block = &buf[offset..offset + bs];
                weak[n] = checksum::of(block) as i32;
                strong[n * cs..(n + 1) * cs].copy_from_slice(&md5(block)[..cs]);
                n += 1;
                offset += bs;
            } else if offset > 0 {
                buf.copy_within(offset..length, 0);
                length -= offset;
                offset = 0;
            } else {
                let l = loop {
                    match input.read(&mut buf[length..]) {
                        Ok(l) => break l,
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                        Err(e) => return Err(io_error("reading", &e)),
                    }
                };
                if l == 0 {
                    break;
                }
                length += l;
            }
        }
        let info = BlockInfo {
            block_size: bs,
            block_count: n,
            checksum_size: cs,
            weak,
            strong,
        };
        info.check("preprocess")?;
        Ok(info)
    }

    /// `rsyncDecompress`, for one file: writes the new file from the
    /// tokens, copying blocks from the old one.
    pub struct Decompressor {
        block_size: usize,
        buf: Vec<u8>,
    }

    impl Decompressor {
        /// A decompressor for blocks of `block_size`.
        pub fn new(block_size: usize) -> Decompressor {
            Decompressor {
                block_size,
                buf: vec![0; 8192],
            }
        }

        /// Decodes a chunk, reading blocks from `reference` and writing to
        /// `output`, and reports its progress; true at the end.
        pub fn chunk<R: Read + Seek>(
            &mut self,
            reference: &mut R,
            output: &mut dyn Write,
            progress: &mut Progress<'_>,
            chunk: &[u8],
        ) -> Result<bool, Error> {
            let mut done = 0;
            let mut pos = 0;
            let mut eof = false;
            while pos < chunk.len() {
                match chunk[pos] {
                    b'S' => {
                        let s = string_token(chunk, pos).ok_or_else(|| corrupt("decompression"))?;
                        output.write_all(s).map_err(|e| io_error("writing", &e))?;
                        done += s.len();
                        pos += s.len() + 3;
                    }
                    b'B' => {
                        let t = chunk
                            .get(pos + 1..pos + 5)
                            .ok_or_else(|| corrupt("decompression"))?;
                        let n = u32::from_le_bytes([t[0], t[1], t[2], 0]) as u64;
                        let k = usize::from(t[3]);
                        let length = k * self.block_size;
                        self.copy(reference, output, n * self.block_size as u64, length)?;
                        done += length;
                        pos += 5;
                    }
                    b'E' => {
                        eof = true;
                        break;
                    }
                    _ => return Err(corrupt("decompression")),
                }
            }
            progress(done)?;
            Ok(eof)
        }

        /// Copies `length` bytes of the old file from `offset`.
        fn copy<R: Read + Seek>(
            &mut self,
            reference: &mut R,
            output: &mut dyn Write,
            offset: u64,
            mut length: usize,
        ) -> Result<(), Error> {
            reference
                .seek(SeekFrom::Start(offset))
                .map_err(|e| io_error("reading", &e))?;
            while length > 0 {
                let l = length.min(self.buf.len());
                // OCaml writes the buffer whatever was read; a block past the
                // old file's end is a corrupted stream.
                if really_read(reference, &mut self.buf[..l])? < l {
                    return Err(corrupt("decompression"));
                }
                output
                    .write_all(&self.buf[..l])
                    .map_err(|e| io_error("writing", &e))?;
                length -= l;
            }
            Ok(())
        }
    }

    /// `hashTableMaxLength`: half the hash table's largest length.
    const HASH_TABLE_MAX_LENGTH: usize = 1024 * 1024;

    /// `computeHashTableLength`: twice the blocks' count, rounded up to a
    /// power of 2 between 32 and [`HASH_TABLE_MAX_LENGTH`].
    fn hash_table_length(block_count: usize) -> usize {
        let mut n2 = 32;
        while n2 < block_count && n2 != HASH_TABLE_MAX_LENGTH {
            n2 *= 2;
        }
        2 * n2
    }

    /// The blocks' weak checksums, as OCaml masks them.
    fn weak(sigs: &BlockInfo, k: usize) -> u32 {
        (sigs.weak[k] as u32) & 0x7fff_ffff
    }

    /// `rsyncCompress`: sends `src_length` bytes of `input` as tokens
    /// against the old file `sigs` describes.
    pub fn compress(
        sigs: &BlockInfo,
        input: &mut dyn Read,
        src_length: Filesize,
        progress: &mut Progress<'_>,
        transmit: &mut Transmit<'_>,
    ) -> Result<(), Error> {
        sigs.check("compression")?;
        let bs = sigs.block_size;
        let cbs = (2 * bs + 8191) & !8191;
        let mut q = TokenQueue::new(bs);

        // `hashSig`: each bucket lists its blocks last first, as OCaml
        // conses them; the first that matches wins.
        let len = hash_table_length(sigs.block_count);
        let mut table: Vec<Vec<(usize, u32)>> = vec![Vec::new(); len];
        for k in 0..sigs.block_count {
            let cs = weak(sigs, k);
            table[cs as usize & (len - 1)].push((k, cs));
        }
        // `sigFilter`.
        let flen = len << 2;
        let mut filter = vec![0u8; flen];
        for k in 0..sigs.block_count {
            let cs = weak(sigs, k);
            filter[(cs as usize >> 5) & (flen - 1)] |= 1 << (cs >> 28);
        }
        let filter_mem = |cs: u32| filter[(cs as usize >> 5) & (flen - 1)] & (1 << (cs >> 28)) != 0;
        let strong_matches = |k: usize, fp: &[u8; 16]| {
            let pos = k * sigs.checksum_size;
            sigs.strong[pos..pos + sigs.checksum_size] == fp[..sigs.checksum_size]
        };

        let mut buf = vec![0; cbs];
        let roll_table = checksum::init(bs);
        let mut cksum = 0;
        let mut outgoing = b' ';
        let (mut offset, mut to_be_sent, mut length) = (cbs, cbs, cbs);
        let mut absolute_pos: Filesize = 0;

        let mut next = Next::Slide(false);
        loop {
            next = match next {
                Next::Slide(miss) => {
                    if offset + bs <= length {
                        if miss {
                            Next::Roll
                        } else {
                            cksum = checksum::of(&buf[offset..offset + bs]);
                            outgoing = buf[offset];
                            Next::Process
                        }
                    } else if length == cbs {
                        if offset > to_be_sent {
                            q.string(&buf[to_be_sent..offset], progress, transmit)?;
                        }
                        let chunk = length - offset;
                        buf.copy_within(offset..length, 0);
                        let rem = src_length - absolute_pos;
                        let avail = cbs - chunk;
                        let want = if rem > cbs as Filesize {
                            avail
                        } else {
                            (rem.max(0) as usize).min(avail)
                        };
                        let l = really_read(input, &mut buf[chunk..chunk + want])?;
                        absolute_pos += l as Filesize;
                        offset = 0;
                        to_be_sent = 0;
                        length = chunk + l;
                        Next::Slide(miss)
                    } else {
                        if length > to_be_sent {
                            q.string(&buf[to_be_sent..length], progress, transmit)?;
                        }
                        q.push_eof(progress, transmit)?;
                        break;
                    }
                }
                Next::Roll => {
                    cksum = checksum::roll(&roll_table, cksum, outgoing, buf[offset + bs - 1]);
                    outgoing = buf[offset];
                    if filter_mem(cksum) {
                        Next::Process
                    } else {
                        miss(&mut offset, bs, length)
                    }
                }
                Next::Process => {
                    let bucket = &table[cksum as usize & (len - 1)];
                    let mut fp = None;
                    let found = bucket.iter().rev().find(|&&(k, cs)| {
                        cs == cksum
                            && strong_matches(
                                k,
                                fp.get_or_insert_with(|| md5(&buf[offset..offset + bs])),
                            )
                    });
                    match found {
                        None => miss(&mut offset, bs, length),
                        Some(&(k, _)) => {
                            // `hit`.
                            if offset > to_be_sent {
                                q.string(&buf[to_be_sent..offset], progress, transmit)?;
                            }
                            to_be_sent = offset + bs;
                            q.block(k, progress, transmit)?;
                            offset += bs;
                            Next::Slide(false)
                        }
                    }
                }
            };
        }
        q.flush(true, progress, transmit)
    }

    /// What the compressor does next.
    enum Next {
        /// `slideWindow`, after a miss or not.
        Slide(bool),
        /// `rollChecksum`.
        Roll,
        /// `processBlock`.
        Process,
    }

    /// `miss`: slides the window one byte on.
    fn miss(offset: &mut usize, bs: usize, length: usize) -> Next {
        *offset += 1;
        if *offset + bs <= length {
            Next::Roll
        } else {
            Next::Slide(true)
        }
    }
}
