//! The transfer module against OCaml's own (gemel-compat's
//! `tests/fixtures/transfer.txt`, from `tools/ocaml-fixtures/gen_transfer.ml`):
//! the rolling checksum, rsync's sizes, the block information, and every
//! chunk of tokens the source sends, for rsync and for plain contents. Then
//! the chunks decode back to the source file.
//!
//! Then whole transfers between two hosts (`copy::file` with a root behind
//! an in-memory connection), in both directions: plain, rsync deltas,
//! without streaming, without rsync, and a transfer that comes out wrong.

mod support;

use std::cell::RefCell;
use std::io::Cursor;
use std::path::Path as StdPath;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use gemel_base::clroot::Clroot;
use gemel_base::path::{Local, Path};
use gemel_base::util::{Error, FileId};
use gemel_compat::umarshal;
use gemel_engine::copy::{self, Replacing, Source};
use gemel_engine::globals::{self, Roots};
use gemel_engine::transfer::{self, checksum, rsync};
use gemel_engine::{negotiate, uutil};
use gemel_fs::fileinfo::{self, FsConfig, Typ};
use gemel_fs::fspath::Fspath;
use gemel_fs::os;
use gemel_fs::system;
use gemel_rpc::host::Host;
use gemel_rpc::registry::{Dispatch, Rpc};
use gemel_rpc::root::{self, Root};
use md5::{Digest, Md5};
use support::{TempDir, block_on, new_features, new_prefs};

const VECTORS: &str = include_str!("../../gemel-compat/tests/fixtures/transfer.txt");

fn md5(s: &[u8]) -> String {
    Md5::digest(s).iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

/// gen_transfer.ml's generator.
fn random(seed: u64, len: usize) -> Vec<u8> {
    let mut x = seed;
    (0..len)
        .map(|_| {
            x = x
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (x >> 33) as u8
        })
        .collect()
}

fn cat(parts: &[&[u8]]) -> Vec<u8> {
    parts.concat()
}

/// gen_transfer.ml's rsync cases: (name, new file, old file).
fn rsync_cases() -> Vec<(&'static str, Vec<u8>, Vec<u8>)> {
    let base = random(10, 300_000);
    let b = &base[..];
    let zeros = |n| vec![0u8; n];
    vec![
        ("identical", base.clone(), base.clone()),
        ("insert-start", cat(&[&random(11, 5000), b]), base.clone()),
        (
            "insert-middle",
            cat(&[&b[..150_000], &random(12, 3000), &b[150_000..]]),
            base.clone(),
        ),
        ("insert-end", cat(&[b, &random(13, 2000)]), base.clone()),
        ("append", cat(&[b, &random(14, 80_000)]), base.clone()),
        (
            "delete-middle",
            cat(&[&b[..100_000], &b[120_000..]]),
            base.clone(),
        ),
        ("different", random(15, 200_000), base.clone()),
        ("shorter", b[..50_000].to_vec(), base.clone()),
        (
            "duplicate-blocks",
            cat(&[&random(16, 3000), &zeros(100_000), &random(17, 3000)]),
            cat(&[&zeros(64_000), &random(18, 10_000), &zeros(32_000)]),
        ),
        ("one-byte-each", b[1..].to_vec(), base.clone()),
        ("small", random(19, 1500), random(19, 800)),
        (
            "big-blocks",
            cat(&[&random(20, 3_000_000), &random(21, 500)]),
            random(20, 3_000_000),
        ),
    ]
}

/// gen_transfer.ml's plain cases: (name, declared length, contents).
fn generic_cases() -> Vec<(String, i64, Vec<u8>)> {
    let mut v: Vec<_> = [0, 1, 8192, 65497, 65498, 200_000]
        .iter()
        .map(|&n| (format!("plain-{n}"), n as i64, random(30, n)))
        .collect();
    v.push(("plain-short-source".into(), 1000, random(31, 500)));
    v
}

/// A case's lines, from its header to `end`.
fn case<'a>(kind: &str, name: &str) -> Vec<Vec<&'a str>> {
    let mut lines = VECTORS.lines().map(|l| l.split('\t').collect::<Vec<_>>());
    lines
        .by_ref()
        .find(|f| f[0] == kind && f[1] == name)
        .unwrap_or_else(|| panic!("{kind} {name}"));
    lines.take_while(|f| f[0] != "end").collect()
}

/// A chunk's tokens summarised as gen_transfer.ml does.
fn summary(chunk: &[u8]) -> String {
    let (mut s, mut b, mut nb, mut e) = (0, 0, 0, 0);
    let mut p = 0;
    while p < chunk.len() {
        match chunk[p] {
            b'S' => {
                s += 1;
                p += 3 + usize::from(u16::from_le_bytes([chunk[p + 1], chunk[p + 2]]));
            }
            b'B' => {
                b += 1;
                nb += usize::from(chunk[p + 4]);
                p += 5;
            }
            _ => {
                e += 1;
                p += 1;
            }
        }
    }
    format!("S{s} B{b}/{nb} E{e}")
}

fn chunk_line(c: &[u8]) -> Vec<String> {
    vec!["chunk".into(), c.len().to_string(), md5(c), summary(c)]
}

#[test]
fn checksums_match_ocaml() {
    let mut n = 0;
    for f in VECTORS.lines().map(|l| l.split('\t').collect::<Vec<_>>()) {
        if f[0] != "checksum" {
            continue;
        }
        let s = unhex(f[1]);
        let len: usize = f[2].parse().unwrap();
        let c = checksum::of(&s[..len]);
        assert_eq!(c.to_string(), f[3], "checksum of {len} bytes");
        let rolled = checksum::roll(&checksum::init(len), c, s[0], s[len]);
        assert_eq!(rolled.to_string(), f[4], "rolled {len} bytes");
        assert_eq!(rolled, checksum::of(&s[1..=len]));
        n += 1;
    }
    assert_eq!(n, 6);
}

#[test]
fn sizes_match_ocaml() {
    let mut n = 0;
    for f in VECTORS.lines().map(|l| l.split('\t').collect::<Vec<_>>()) {
        if f[0] != "sizes" {
            continue;
        }
        let (src, dst): (i64, i64) = (f[1].parse().unwrap(), f[2].parse().unwrap());
        let want: (usize, usize, usize) = (
            f[3].parse().unwrap(),
            f[4].parse().unwrap(),
            f[5].parse().unwrap(),
        );
        assert_eq!(rsync::sizes(src, dst), want, "sizes {src} {dst}");
        n += 1;
    }
    assert!(n > 80, "{n}");
}

fn no_progress(_: usize) -> Result<(), Error> {
    Ok(())
}

#[test]
fn rsync_matches_ocaml_chunk_by_chunk() {
    for (name, src, dst) in rsync_cases() {
        let lines = case("case", name);
        let header = VECTORS
            .lines()
            .find(|l| l.starts_with(&format!("case\t{name}\t")))
            .unwrap();
        assert_eq!(
            header,
            format!(
                "case\t{name}\t{}\t{}\t{}\t{}",
                src.len(),
                md5(&src),
                dst.len(),
                md5(&dst)
            ),
            "{name}: the inputs"
        );
        let (src_len, dst_len) = (src.len() as i64, dst.len() as i64);
        let info = rsync::preprocess(&mut Cursor::new(&dst), src_len, dst_len).unwrap();
        let marshalled = umarshal::to_bytes(|w| info.marshal(w));
        assert_eq!(
            lines[0],
            [
                "info".to_string(),
                info.block_size.to_string(),
                info.block_count.to_string(),
                info.checksum_size.to_string(),
                marshalled.len().to_string(),
                md5(&marshalled),
            ],
            "{name}: the block information"
        );
        assert_eq!(
            umarshal::from_bytes(&marshalled, rsync::BlockInfo::unmarshal).unwrap(),
            info
        );

        let mut chunks = Vec::new();
        let mut progress = 0;
        rsync::compress(
            &info,
            &mut Cursor::new(&src),
            src_len,
            &mut |n| {
                progress += n;
                Ok(())
            },
            &mut |c| {
                chunks.push(c.to_vec());
                Ok(())
            },
        )
        .unwrap();
        let got: Vec<Vec<String>> = chunks.iter().map(|c| chunk_line(c)).collect();
        assert_eq!(got, lines[1..], "{name}: the chunks");

        // The destination rebuilds the new file from the old one.
        let mut d = rsync::Decompressor::new(info.block_size);
        let mut out = Vec::new();
        let mut reference = Cursor::new(&dst);
        let mut done = 0;
        let mut ends = Vec::new();
        for c in &chunks {
            ends.push(
                d.chunk(
                    &mut reference,
                    &mut out,
                    &mut |n| {
                        done += n;
                        Ok(())
                    },
                    c,
                )
                .unwrap(),
            );
        }
        assert_eq!(ends.pop(), Some(true), "{name}");
        assert!(ends.iter().all(|e| !e), "{name}");
        assert!(out == src, "{name}: decompressed");
        assert_eq!(done, src.len(), "{name}");
        // The source counts blocks whole, the tail included.
        assert!(progress >= src.len() - info.block_size, "{name}");
    }
}

#[test]
fn plain_contents_match_ocaml_chunk_by_chunk() {
    for (name, declared, contents) in generic_cases() {
        let header = VECTORS
            .lines()
            .find(|l| l.starts_with(&format!("generic\t{name}\t")))
            .unwrap();
        assert_eq!(
            header,
            format!(
                "generic\t{name}\t{declared}\t{}\t{}",
                contents.len(),
                md5(&contents)
            )
        );
        let lines = case("generic", &name);
        let mut chunks = Vec::new();
        transfer::send(
            &mut Cursor::new(&contents),
            declared,
            &mut no_progress,
            &mut |c| {
                chunks.push(c.to_vec());
                Ok(())
            },
        )
        .unwrap();
        let got: Vec<Vec<String>> = chunks.iter().map(|c| chunk_line(c)).collect();
        assert_eq!(got, lines, "{name}");
        let mut out = Vec::new();
        let ends: Vec<bool> = chunks
            .iter()
            .map(|c| transfer::receive(&mut out, &mut no_progress, c).unwrap())
            .collect();
        assert_eq!(ends.last(), Some(&true), "{name}");
        assert_eq!(out, contents, "{name}");
    }
}

/// A stream that isn't one is an error, not a panic.
#[test]
fn bad_streams_are_errors() {
    let mut out = Vec::new();
    for chunk in [&b"X"[..], b"S\x05\x00ab", b"B\x00\x00"] {
        assert!(matches!(
            transfer::receive(&mut out, &mut no_progress, chunk),
            Err(Error::Transient(_))
        ));
        let mut d = rsync::Decompressor::new(1024);
        assert!(matches!(
            d.chunk(
                &mut Cursor::new(vec![0; 2048]),
                &mut out,
                &mut no_progress,
                chunk
            ),
            Err(Error::Transient(_))
        ));
    }
    // A block past the end of the old file.
    let mut d = rsync::Decompressor::new(1024);
    assert!(
        d.chunk(
            &mut Cursor::new(vec![0; 2048]),
            &mut out,
            &mut no_progress,
            b"B\x02\x00\x00\x01E"
        )
        .is_err()
    );
    // Block information whose counts don't fit its checksums.
    let info = rsync::BlockInfo {
        block_size: 1024,
        block_count: 3,
        checksum_size: 2,
        weak: vec![0; 2],
        strong: vec![0; 6],
    };
    let e = rsync::compress(
        &info,
        &mut Cursor::new(vec![0; 4096]),
        4096,
        &mut no_progress,
        &mut |_| Ok(()),
    )
    .unwrap_err();
    assert!(
        e.to_string()
            .starts_with("Internal error during rsync transfer (compression)"),
        "{e}"
    );
}

// ---- Between two hosts ----

/// A client with root `a` here and root `b` on a server named "server",
/// behind an in-memory connection; and the progress the client saw.
struct Hosts {
    rpc: Rpc,
    roots: Roots,
    server: Rc<Host>,
    dir: TempDir,
    progress: Arc<Mutex<Vec<(i64, String)>>>,
}

fn hosts(tag: &str, prefs: &[&str]) -> Hosts {
    let dir = TempDir::new(tag);
    for d in ["a", "b", "u", "v"] {
        std::fs::create_dir_all(dir.0.join(d)).unwrap();
    }
    let fspath = |d: &str| {
        let bytes = system::name_bytes(dir.0.join(d).as_os_str());
        Fspath::canonize(Some(&bytes)).unwrap()
    };
    let mut p = new_prefs();
    p.parse_cmd_line(&prefs.iter().map(|s| s.to_string()).collect::<Vec<_>>())
        .unwrap();
    let client = Rc::new(Host::client(
        Rc::new(RefCell::new(p)),
        Rc::new(RefCell::new(new_features())),
    ));
    client.set_unison_dir(dir.0.join("u"));
    let progress = Arc::new(Mutex::new(Vec::new()));
    let seen = progress.clone();
    uutil::set_progress_printer(
        &client,
        Some(Arc::new(move |_, n, how| {
            seen.lock().unwrap().push((n, how.to_owned()))
        })),
    );
    let mut rpc = Rpc::new(Dispatch::Direct(client));
    gemel_engine::register_commands(&mut rpc).unwrap();
    let server = Rc::new(Host::server(new_prefs(), new_features()));
    server.set_host_name("server");
    server.set_unison_dir(dir.0.join("v"));
    let b = Root {
        host: root::Host::Remote("server".into()),
        fspath: fspath("b"),
    };
    let clroot = Clroot::ConnectBySocket {
        host: "server".into(),
        port: "1".into(),
        path: None,
    };
    let roots = Roots::from_roots(vec![Root::local(fspath("a")), b.clone()]);
    block_on(async {
        rpc.connect_in_memory(clroot, b, server.clone());
        negotiate::features(&rpc, &roots.in_canonical_order())
            .await
            .unwrap();
        globals::propagate_prefs(&rpc, &roots).await.unwrap();
    });
    Hosts {
        rpc,
        roots,
        server,
        dir,
        progress,
    }
}

impl Hosts {
    fn root(&self, side: &str) -> &Root {
        &self.roots.list()[if side == "a" { 0 } else { 1 }]
    }

    fn path(&self, side: &str, name: &str) -> std::path::PathBuf {
        self.dir.0.join(side).join(name)
    }

    /// The archive's record of a file, as update detection would make it.
    fn source(&self, side: &str, name: &str) -> Source {
        let fs = FsConfig::from_prefs(&self.rpc.client_host().prefs().borrow());
        let root = self.root(side);
        let p = local(name);
        let info = fileinfo::get(true, &root.fspath, &p, &|_| false, &fs).unwrap();
        let fp = os::fingerprint(&root.fspath, &p, info.typ.info_kind(), false).unwrap();
        Source {
            desc: info.desc.clone(),
            fp,
            stamp: None,
            ress: info.ress_stamp(),
        }
    }

    /// Transfers `from`'s `name` to a temporary file on the other root,
    /// replacing its `name` if `update`.
    fn transfer(&self, from: &str, name: &str, update: bool) -> Result<fileinfo::Info, Error> {
        let to = if from == "a" { "b" } else { "a" };
        let source = self.source(from, name);
        let replacing = if update {
            Replacing::Update(
                std::fs::metadata(self.path(to, name)).unwrap().len() as i64,
                0,
            )
        } else {
            Replacing::Copy
        };
        self.transfer_with(from, name, replacing, &source)
    }

    fn transfer_with(
        &self,
        from: &str,
        name: &str,
        replacing: Replacing,
        source: &Source,
    ) -> Result<fileinfo::Info, Error> {
        let to = if from == "a" { "b" } else { "a" };
        let (root_from, root_to) = (self.root(from), self.root(to));
        block_on(copy::file(
            &self.rpc,
            root_from,
            &local(name),
            root_to,
            &root_to.fspath,
            &local(&format!(".tmp-{name}")),
            &local(name),
            replacing,
            source,
            FileId(7),
        ))
    }

    /// The bytes that crossed the connection so far, both ways.
    fn wire(&self) -> u64 {
        let c = self.rpc.connection_of_root(self.root("b")).unwrap();
        c.bytes_emitted() + c.bytes_received()
    }

    /// The bytes the client's progress printer saw, and how.
    fn take_progress(&self) -> (i64, Vec<String>) {
        let mut p = self.progress.lock().unwrap();
        let total = p.iter().map(|(n, _)| n).sum();
        let mut how: Vec<String> = p.drain(..).map(|(_, h)| h).collect();
        how.dedup();
        (total, how)
    }
}

fn local(s: &str) -> Path<Local> {
    Path::from_bytes(s.as_bytes()).unwrap()
}

fn write_file(p: &StdPath, contents: &[u8]) {
    std::fs::write(p, contents).unwrap();
}

/// A file's name, its contents, and the old version it replaces, if any.
type Case = (&'static str, Vec<u8>, Option<Vec<u8>>);

/// Each size class, both ways: below the rsync threshold, a new file,
/// deltas against an old version, an append, and a file replaced by one
/// with nothing in common.
#[test]
fn files_transfer_between_hosts_both_ways() {
    for prefs in [
        &[][..],
        &["-stream=false"],
        &["-rsync=false"],
        &["-halfduplex=true"],
    ] {
        let h = hosts("two-hosts", prefs);
        let base = random(40, 300_000);
        for (from, to) in [("a", "b"), ("b", "a")] {
            let cases: Vec<Case> = vec![
                ("small", random(41, 500), None),
                ("new", random(42, 200_000), None),
                (
                    "edited",
                    cat(&[&base[..100_000], &random(43, 777), &base[100_000..]]),
                    Some(base.clone()),
                ),
                (
                    "appended",
                    cat(&[&base, &random(44, 50_000)]),
                    Some(base.clone()),
                ),
                ("replaced", random(45, 250_000), Some(base.clone())),
                ("empty", Vec::new(), Some(base.clone())),
                // Many chunks, well over the pipe's size.
                ("big", random(46, 3_000_000), None),
            ];
            for (name, contents, old) in cases {
                let name = format!("{name}-{from}");
                write_file(&h.path(from, &name), &contents);
                if let Some(old) = &old {
                    write_file(&h.path(to, &name), old);
                }
                let wire = h.wire();
                let info = h
                    .transfer(from, &name, old.is_some())
                    .unwrap_or_else(|e| panic!("{name} {prefs:?}: {e}"));
                let wire = h.wire() - wire;
                // Deltas go when rsync is on and the files share most of
                // their blocks; otherwise the whole file does.
                // (In half duplex, idle sides hand the write token back and
                // forth, as OCaml's do, which adds bytes as time passes.)
                let rsync = !prefs.contains(&"-rsync=false");
                let half_duplex = prefs.contains(&"-halfduplex=true");
                let delta = rsync && (name.starts_with("edited") || name.starts_with("appended"));
                if delta && !half_duplex {
                    let new_bytes = contents.len() - base.len().min(contents.len());
                    assert!(
                        wire < (new_bytes + 20_000) as u64,
                        "{name} {prefs:?}: {wire} bytes"
                    );
                } else if !delta {
                    assert!(
                        wire >= contents.len() as u64,
                        "{name} {prefs:?}: {wire} bytes"
                    );
                }
                assert_eq!(info.typ, Typ::File, "{name}");
                let tmp = h.path(to, &format!(".tmp-{name}"));
                assert!(
                    std::fs::read(&tmp).unwrap() == contents,
                    "{name} {prefs:?}: the copy"
                );
                // The old version is still there, untouched.
                if let Some(old) = &old {
                    assert!(std::fs::read(h.path(to, &name)).unwrap() == *old, "{name}");
                }
                // The client sees the transfer's progress (whichever host
                // sends or receives), as "r", for the whole file.
                // (An empty file has nothing to report.)
                let (total, how) = h.take_progress();
                if !contents.is_empty() {
                    assert_eq!(how, ["r"], "{name}");
                }
                assert!(total >= contents.len() as i64, "{name}: {total}");
            }
        }
        // The server noted where it got each file, for later copies.
        assert!(gemel_engine::xferhint::lookup(&h.server, &h.source("a", "new-a").fp).is_some());
    }
}

/// A transfer that doesn't match the source's record fails; the source is
/// checked, on its host, and the error says it changed.
#[test]
fn a_transfer_whose_source_changed_fails() {
    for from in ["a", "b"] {
        let h = hosts("two-hosts-bad", &[]);
        write_file(&h.path(from, "f"), &random(50, 5000));
        let mut source = h.source(from, "f");
        // As if the file had changed after update detection: the source
        // check reports that.
        write_file(&h.path(from, "g"), &random(51, 5000));
        source.fp = h.source(from, "g").fp;
        let e = h
            .transfer_with(from, "f", Replacing::Copy, &source)
            .unwrap_err();
        assert!(
            e.to_string()
                .contains("has been modified during synchronization"),
            "{from}: {e}"
        );
    }
}

/// A file already transferred (an interrupted sync left the temporary file
/// whole) only gets its props, and the log says so.
#[test]
fn a_file_already_transferred_is_not_sent_again() {
    let h = hosts("two-hosts-done", &[]);
    let contents = random(60, 100_000);
    write_file(&h.path("a", "f"), &contents);
    write_file(&h.path("b", ".tmp-f"), &contents);
    h.transfer("a", "f", false).unwrap();
    let (total, how) = h.take_progress();
    assert_eq!(
        (total, how),
        (contents.len() as i64, vec!["rem".to_owned()])
    );
}

// ---- Against OCaml's own sessions ----

const CAPTURED: &str = include_str!("captured/transfers.txt");

/// The features a captured sync negotiated, from its `negotiateFeatures`
/// reply.
fn captured_features(session: &str) -> gemel_base::features::Features {
    let reply = CAPTURED
        .lines()
        .map(|l| l.split('\t').collect::<Vec<_>>())
        .find(|f| f[0] == session && f[2] == "res" && f[3] == "negotiateFeatures")
        .unwrap();
    let names =
        gemel_rpc::protocol::decode_section_body("negotiateFeatures-res", &unhex(reply[4]), |r| {
            r.list(|r| Ok(String::from_utf8(r.string()?).unwrap()))
        })
        .unwrap();
    let mut f = new_features();
    f.set_enabled(&names);
    f
}

/// Every transfer section OCaml sent, decoded with gemel's codecs and
/// encoded again, comes out the same: `transferFile`'s argument and
/// result, `compress`'s (with and without block information), the stream
/// chunks and their pings.
#[test]
fn ocamls_transfer_messages_decode_and_encode_the_same() {
    let mut seen = std::collections::BTreeSet::new();
    for line in CAPTURED.lines().filter(|l| !l.starts_with('#')) {
        let f: Vec<&str> = line.split('\t').collect();
        let (session, kind, name, body) = (f[0], f[2], f[3], unhex(f[4]));
        let features = captured_features(session);
        // A section is its length, then its body.
        let again = match (kind, name) {
            ("args", "transferFile") => {
                let a = copy::TRANSFER_FILE.decode_arg(&body, &features).unwrap();
                copy::TRANSFER_FILE.encode_arg(&a, &features)
            }
            ("res", "transferFile") => {
                let r = copy::TRANSFER_FILE.decode_res(&body, &features).unwrap();
                copy::TRANSFER_FILE.encode_res(&r, &features)
            }
            ("args", "compress") => {
                let a = copy::COMPRESS.decode_arg(&body, &features).unwrap();
                if session == "update" {
                    assert!(a.block_info.is_some(), "{line:.80}");
                }
                copy::COMPRESS.encode_arg(&a, &features)
            }
            ("res", "compress") => {
                copy::COMPRESS.decode_res(&body, &features).unwrap();
                copy::COMPRESS.encode_res(&(), &features)
            }
            ("str", "processTransferInstruction") => {
                let c = copy::PROCESS_TRANSFER_INSTRUCTION
                    .decode_chunk(&body)
                    .unwrap();
                copy::PROCESS_TRANSFER_INSTRUCTION.encode_chunk(&c)
            }
            ("args", "processTransferInstructionPing") => {
                let tag = "processTransferInstructionPing-args";
                let id = gemel_rpc::protocol::decode_section_body(tag, &body, |r| r.int()).unwrap();
                gemel_rpc::protocol::encode_section(tag, |w| w.int(id))
            }
            ("res", "processTransferInstructionPing") => {
                let tag = "processTransferInstructionPing-res";
                gemel_rpc::protocol::decode_section_body(tag, &body, |r| r.unit()).unwrap();
                gemel_rpc::protocol::encode_section(tag, |w| w.unit())
            }
            ("args" | "res", "negotiateFeatures") => continue,
            other => panic!("{other:?}"),
        }
        .unwrap();
        assert_eq!(again[5..], body[..], "{session} {kind} {name}");
        seen.insert((session, kind, name));
    }
    assert_eq!(seen.len(), 14, "{seen:?}");
}

/// A resource fork goes after the data fork, both ways (macOS).
#[cfg(target_os = "macos")]
#[test]
fn resource_forks_transfer_between_hosts() {
    let h = hosts("two-hosts-ress", &["-rsrc=true"]);
    gemel_fs::osx::init(&mut h.rpc.client_host().prefs().borrow_mut(), true);
    block_on(globals::propagate_prefs(&h.rpc, &h.roots)).unwrap();
    assert!(*h.server.prefs().borrow().read(&gemel_fs::osx::RSRC_AUX));
    for (from, to) in [("a", "b"), ("b", "a")] {
        let name = format!("forked-{from}");
        let fork = random(70, 3000);
        write_file(&h.path(from, &name), b"the data fork");
        write_file(&h.path(from, &format!("{name}/..namedfork/rsrc")), &fork);
        let source = {
            let fs = FsConfig::from_prefs(&h.rpc.client_host().prefs().borrow());
            let root = h.root(from);
            let info = fileinfo::get(true, &root.fspath, &local(&name), &|_| false, &fs).unwrap();
            let fp =
                os::fingerprint(&root.fspath, &local(&name), info.typ.info_kind(), true).unwrap();
            Source {
                desc: info.desc.clone(),
                fp,
                stamp: None,
                ress: info.ress_stamp(),
            }
        };
        assert_eq!(gemel_fs::osx::ress_length(&source.ress), 3000);
        h.transfer_with(from, &name, Replacing::Copy, &source)
            .unwrap_or_else(|e| panic!("{from}: {e}"));
        let tmp = h.path(to, &format!(".tmp-{name}"));
        assert_eq!(std::fs::read(&tmp).unwrap(), b"the data fork");
        assert!(
            std::fs::read(tmp.join("..namedfork/rsrc")).unwrap() == fork,
            "{from}: the resource fork"
        );
    }
}

/// Aborting everything partway through stops a transfer, whichever host
/// sends: the client's own reading or decoding fails, or the server stops
/// streaming once the client aborts the stream. Nothing waits forever, and
/// the connection serves the next transfer.
#[test]
fn an_abort_stops_a_transfer_both_ways() {
    for (from, to) in [("a", "b"), ("b", "a")] {
        let h = hosts("two-hosts-abort", &[]);
        let client = h.rpc.client_host().clone();
        let flag = client
            .state::<gemel_engine::abort::Abort>()
            .borrow()
            .all_flag();
        let total = Arc::new(Mutex::new(0i64));
        let t = total.clone();
        uutil::set_progress_printer(
            &client,
            Some(Arc::new(move |_, n, _| {
                let mut t = t.lock().unwrap();
                *t += n;
                if *t > 1_000_000 {
                    flag.set();
                }
            })),
        );
        let name = format!("big-{from}");
        write_file(&h.path(from, &name), &random(80, 5_000_000));
        let e = h.transfer(from, &name, false).unwrap_err();
        assert!(gemel_engine::abort::is_abort(&e), "{from}: {e}");
        let sent = *total.lock().unwrap();
        assert!(sent < 5_000_000, "{from}: {sent}");
        // Nothing is aborted any more: the next transfer goes through.
        client
            .state::<gemel_engine::abort::Abort>()
            .borrow_mut()
            .reset();
        uutil::set_progress_printer(&client, None);
        write_file(&h.path(from, "after"), b"after the abort");
        h.transfer(from, "after", false).unwrap();
        assert_eq!(
            std::fs::read(h.path(to, ".tmp-after")).unwrap(),
            b"after the abort"
        );
    }
}
