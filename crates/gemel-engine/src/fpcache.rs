//! The fingerprint cache.
//!
//! Ports Unison `src/fpcache.ml`.
//!
//! Derived from Unison, Copyright 1999-2020, Benjamin C. Pierce.
//!
//! Update detection records each fingerprint it computes in a file in the
//! Unison directory (`fp…`, named like the archive), so that an interrupted
//! scan doesn't have to fingerprint the same files again. The next scan
//! loads it and reuses an entry if the file is clearly unchanged.
//!
//! The file is OCaml's, byte for byte: the line [`MAGIC`], then chunks of
//! entries. A chunk is the MD5 of the Umarshal header, the MD5 of the whole
//! frame, then the frame: a list of (prefix length, rest of the path,
//! entry), each path given as the length of the prefix it shares with the
//! previous one and the rest. Reading stops quietly at the first damaged
//! chunk. A chunk is written every 5000 entries or 100 MB of files, and at
//! the end; the file is only created (and truncated) by the first write, so
//! a scan that fingerprints nothing leaves the previous cache in place.
//!
//! OCaml keeps one cache in globals, which works because it scans one root
//! after the other. Here a [`FpCache`] is an owned value, one per root, that
//! a scan takes with it to a blocking thread.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Write as _};
use std::path::{Path as StdPath, PathBuf};

use gemel_base::path::{Local, Path};
use gemel_base::prefs::{Category, PrefDef, Prefs};
use gemel_base::util::Error;
use gemel_compat::umarshal::{self, HEADER_SIZE, Reader, Writer};
use gemel_fs::fileinfo::{self, FsConfig, Info, Stamp};
use gemel_fs::os::{self, FullFingerprint};
use gemel_fs::osx::{self, RessStamp};
use gemel_fs::props::{Filesize, Props, PropsConfig, XattrCtx};
use md5::{Digest, Md5};

/// The first line of the cache file.
pub const MAGIC: &str = "Unison fingerprint cache format 3";

/// What the cache records for a file: its props, fingerprint, stamp and
/// resource fork stamp, as of the fingerprint.
pub type Entry = (Props, FullFingerprint, Stamp, RessStamp);

/// A chunk of the file: entries with their full paths.
pub type Chunk = Vec<(Vec<u8>, Entry)>;

const MAX_COUNT: usize = 5000;
const MAX_SIZE: Filesize = 100 * 1024 * 1024;

/// `fastercheckUNSAFE`.
pub static FASTER_CHECK_UNSAFE: PrefDef<bool> = PrefDef::new(
    "fastercheckUNSAFE",
    Category::Expert,
    || false,
    "skip computing fingerprints for new files (experts only!)",
    "THIS FEATURE IS STILL EXPERIMENTAL AND SHOULD BE USED WITH EXTREME CAUTION.  \
     \n\n\
     When this flag is set to {\\tt true}, Unison will compute a 'pseudo-\
     fingerprint' the first time it sees a file (either because the file is \
     new or because Unison is running for the first time).  This enormously \
     speeds update detection, but it must be used with care, as it can cause \
     Unison to miss conflicts: If \
     a given path in the filesystem contains files on {\\em both} sides that \
     Unison has not yet seen, and if those files have the same length but different \
     contents, then Unison will not notice the presence of a conflict.  If, later, one \
     of the files is changed, the changed file will be propagated, overwriting  \
     the other.  \
     \n\n\
     Moreover, even when the files are initially identical, setting this flag can lead \
     to potentially confusing behavior: \
     if a newly created file is later touched without being modified, Unison will \
     treat this \
     conservatively as a potential change (since it has no record of the earlier \
     contents) and show it as needing to be propagated to the other replica. \
     \n\n\
     Most users should leave this flag off -- the small time savings of not \
     fingerprinting new files is not worth the cost in terms of safety.  However, \
     it can be very useful for power users with huge replicas that are known to \
     be already synchronized (e.g., because one replica is a newly created duplicate \
     of the other, or because they have previously been synchronized with Unison but \
     Unison's archives need to be rebuilt).  In such situations, it is recommended \
     that this flag be set only for the initial run of Unison, so that new archives \
     can be created quickly, and then turned off for normal use.",
);

/// Registers this module's preferences.
pub fn register_prefs(prefs: &mut Prefs) -> Result<(), Error> {
    prefs.register(&FASTER_CHECK_UNSAFE)
}

fn marshal_entry(
    w: &mut Writer,
    (i, s, (desc, fp, stamp, ress)): &(i64, Vec<u8>, Entry),
    c: &PropsConfig,
) {
    w.int(*i);
    w.string(s);
    desc.marshal(w, c);
    fp.marshal(w);
    stamp.marshal(w);
    ress.marshal_stamp(w);
}

fn unmarshal_entry(r: &mut Reader<'_>, c: &PropsConfig) -> umarshal::Result<(i64, Vec<u8>, Entry)> {
    Ok((
        r.int()?,
        r.string()?,
        (
            Props::unmarshal(r, c)?,
            FullFingerprint::unmarshal(r)?,
            Stamp::unmarshal(r)?,
            RessStamp::unmarshal_stamp(r)?,
        ),
    ))
}

/// `decompress`: the path from the previous one and a delta, or `None` if
/// the delta doesn't fit.
fn decompress(prev: &mut Vec<u8>, i: i64, rest: &[u8]) -> Option<Vec<u8>> {
    let i = usize::try_from(i).ok().filter(|&i| i <= prev.len())?;
    let s = [&prev[..i], rest].concat();
    prev.clone_from(&s);
    Some(s)
}

/// Reads the cache file's chunks, each as its entries with their full
/// paths, stopping at the first damaged one (`init`'s reading loop).
/// Fails only if the file can't be opened or lacks the magic line.
pub fn read_file(file: &StdPath, c: &PropsConfig) -> std::io::Result<Vec<Chunk>> {
    let mut input = BufReader::new(File::open(file)?);
    let mut header = Vec::new();
    input.read_until(b'\n', &mut header)?;
    if header.strip_suffix(b"\n").unwrap_or(&header) != MAGIC.as_bytes() {
        return Err(std::io::Error::other("wrong header"));
    }
    let mut chunks = Vec::new();
    let mut prev = Vec::new();
    while let Some(chunk) = read_chunk(&mut input, &mut prev, c) {
        let complete = chunk.1;
        chunks.push(chunk.0);
        if !complete {
            break;
        }
    }
    Ok(chunks)
}

/// `read`: one chunk, and whether all of it could be decompressed. `None`
/// at the end of the file or for a damaged chunk.
fn read_chunk(input: &mut impl Read, prev: &mut Vec<u8>, c: &PropsConfig) -> Option<(Chunk, bool)> {
    let mut fp1 = [0; 16];
    let mut fp2 = [0; 16];
    input.read_exact(&mut fp1).ok()?;
    input.read_exact(&mut fp2).ok()?;
    let mut frame = vec![0; HEADER_SIZE];
    input.read_exact(&mut frame).ok()?;
    if Md5::digest(&frame)[..] != fp1 {
        return None;
    }
    let size = umarshal::data_size(&frame).ok()?;
    frame.resize(HEADER_SIZE + size, 0);
    input.read_exact(&mut frame[HEADER_SIZE..]).ok()?;
    if Md5::digest(&frame)[..] != fp2 {
        return None;
    }
    let q = umarshal::from_bytes(&frame, |r| r.list(|r| unmarshal_entry(r, c))).ok()?;
    let mut entries = Vec::with_capacity(q.len());
    for (i, rest, entry) in q {
        let Some(path) = decompress(prev, i, &rest) else {
            return Some((entries, false));
        };
        entries.push((path, entry));
    }
    Some((entries, true))
}

enum Output {
    /// Not opened yet: opened (and truncated) by the first write.
    Closed,
    Open(File),
}

/// Where new fingerprints are written (`state`).
struct Out {
    file: PathBuf,
    output: Output,
    count: usize,
    size: Filesize,
    last: Vec<u8>,
    queue: Vec<(i64, Vec<u8>, Entry)>,
}

/// A root's fingerprint cache.
pub struct FpCache {
    table: HashMap<Vec<u8>, Entry>,
    out: Option<Out>,
    props: PropsConfig,
}

impl std::fmt::Debug for FpCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FpCache")
            .field("entries", &self.table.len())
            .field("writing", &self.out.as_ref().map(|o| &o.file))
            .finish_non_exhaustive()
    }
}

impl FpCache {
    /// No cache: nothing is found, nothing is written.
    pub fn disabled(props: PropsConfig) -> FpCache {
        FpCache {
            table: HashMap::new(),
            out: None,
            props,
        }
    }

    /// `Fpcache.init`: loads the cache `file` and records new fingerprints
    /// in it, if fast checking is on and archives aren't ignored. `props`
    /// encodes the props (only its feature flags matter).
    pub fn init(
        fast_check: bool,
        ignore_archives: bool,
        file: &StdPath,
        props: PropsConfig,
    ) -> FpCache {
        let mut cache = FpCache::disabled(props);
        if !fast_check || ignore_archives {
            return cache;
        }
        // Errors only mean there is nothing to reuse.
        if let Ok(chunks) = read_file(file, &props) {
            cache.table.extend(chunks.into_iter().flatten());
        }
        cache.out = Some(Out {
            file: file.to_owned(),
            output: Output::Closed,
            count: 0,
            size: 0,
            last: Vec::new(),
            queue: Vec::new(),
        });
        cache
    }

    /// The cached entry for a path, if any.
    pub fn get(&self, path: &Path<Local>) -> Option<&Entry> {
        self.table.get(path.as_bytes())
    }

    /// `Fpcache.finish`: writes what is left and closes the file.
    pub fn finish(mut self) {
        if let Some(out) = &self.out
            && !out.queue.is_empty()
        {
            self.write();
        }
    }

    /// `write`: writes the queued entries as a chunk, opening the file if
    /// this is the first write. On an error, stops recording.
    fn write(&mut self) {
        let Some(out) = &mut self.out else {
            return;
        };
        if matches!(out.output, Output::Closed) {
            match open_cache_file(&out.file) {
                Ok(f) => out.output = Output::Open(f),
                Err(_) => {
                    self.out = None;
                    return;
                }
            }
        }
        let Output::Open(f) = &mut out.output else {
            unreachable!("opened above")
        };
        let queue = std::mem::take(&mut out.queue);
        let s = umarshal::to_bytes(|w| w.list(&queue, |w, e| marshal_entry(w, e, &self.props)));
        let fp1 = Md5::digest(&s[..HEADER_SIZE]);
        let fp2 = Md5::digest(&s);
        let r = f
            .write_all(&fp1)
            .and_then(|()| f.write_all(&fp2))
            .and_then(|()| f.write_all(&s))
            .and_then(|()| f.flush());
        out.count = 0;
        out.size = 0;
        if r.is_err() {
            self.out = None;
        }
    }

    /// `Fpcache.save`: records a fingerprint.
    pub fn save(&mut self, path: &Path<Local>, v: Entry) {
        let Some(out) = &mut self.out else {
            return;
        };
        out.size += v.0.length();
        out.count += 1;
        // compress: the length of the prefix shared with the previous path.
        let p = path.as_bytes();
        let i = p.iter().zip(&out.last).take_while(|(a, b)| a == b).count();
        out.last = p.to_vec();
        out.queue.push((i as i64, p[i..].to_vec(), v));
        if out.count > MAX_COUNT || out.size > MAX_SIZE {
            self.write();
        }
    }

    /// `Fpcache.fingerprint`: the file's fingerprint, from the cache if the
    /// file is clearly unchanged since, else computed (with
    /// `Os.safeFingerprint`, or as a pseudo-fingerprint for a new file under
    /// `fastercheckUNSAFE`). Records it in any case.
    #[allow(clippy::too_many_arguments)]
    pub fn fingerprint(
        &mut self,
        new_file: bool,
        fast_check: bool,
        faster_check_unsafe: bool,
        fspath: &gemel_fs::fspath::Fspath,
        path: &Path<Local>,
        info: &Info,
        known: Option<&FullFingerprint>,
        follow: &dyn Fn(&Path<Local>) -> bool,
        c: &FsConfig,
        x: &XattrCtx<'_>,
    ) -> Result<Entry, Error> {
        let cached = self.get(path).filter(|(desc, _, stamp, ress)| {
            clearly_unchanged(fast_check, path, info, desc, *stamp, ress, &c.props)
        });
        let res = match cached {
            Some((_, fp, _, _)) => (
                info.desc.clone(),
                fp.clone(),
                info.stamp(c),
                info.ress_stamp(),
            ),
            None => {
                let (info, dig) = if faster_check_unsafe && new_file {
                    // The length is the scan's, as in OCaml, not the new stat's.
                    let dig = FullFingerprint::pseudo(path.as_bytes(), info.desc.length());
                    let info = fileinfo::get(false, fspath, path, follow, c, &Props::dummy(), x)?;
                    (info, dig)
                } else {
                    os::safe_fingerprint(
                        fspath,
                        path,
                        info,
                        known.map(|f| f.data.clone()),
                        follow,
                        c,
                        x,
                    )?
                };
                let stamp = info.stamp(c);
                let ress = info.ress_stamp();
                (info.desc, dig, stamp, ress)
            }
        };
        self.save(path, res.clone());
        Ok(res)
    }
}

/// `open_cache_file`: creates the file, with the magic line.
fn open_cache_file(file: &StdPath) -> std::io::Result<File> {
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
    let mut f = opts.open(file)?;
    f.write_all(MAGIC.as_bytes())?;
    f.write_all(b"\n")?;
    f.flush()?;
    Ok(f)
}

/// `Fpcache.excelFile`: a file whose modification time Excel (or Project)
/// may not update, so fast checking is disabled for it.
pub fn excel_file(path: &Path<Local>) -> bool {
    let s = path.as_bytes();
    s.ends_with(b".xls") || s.ends_with(b".mpp")
}

/// `Fpcache.dataClearlyUnchanged`: whether the file's data is unchanged
/// since `desc` and `stamp` were recorded, judging by its time, length and
/// inode.
pub fn data_clearly_unchanged(
    fast_check: bool,
    path: &Path<Local>,
    info: &Info,
    desc: &Props,
    stamp: Stamp,
    c: &PropsConfig,
) -> bool {
    fast_check
        && info.desc.same_time(desc, c)
        && info.desc.length() == desc.length()
        && !excel_file(path)
        && match stamp {
            Stamp::Inode(inode) => info.inode == inode,
            Stamp::NoStamp => true,
            Stamp::Rescan => false,
        }
}

/// `Fpcache.ressClearlyUnchanged`.
pub fn ress_clearly_unchanged(
    fast_check: bool,
    info: &Info,
    ress: &RessStamp,
    data_clearly_unchanged: bool,
) -> bool {
    fast_check && osx::ress_unchanged(ress, &info.os_x.ress_info, None, data_clearly_unchanged)
}

fn clearly_unchanged(
    fast_check: bool,
    path: &Path<Local>,
    info: &Info,
    desc: &Props,
    stamp: Stamp,
    ress: &RessStamp,
    c: &PropsConfig,
) -> bool {
    let du = data_clearly_unchanged(fast_check, path, info, desc, stamp, c);
    du && ress_clearly_unchanged(fast_check, info, ress, du)
}
