//! The transfer module against OCaml's own (gemel-compat's
//! `tests/fixtures/transfer.txt`, from `tools/ocaml-fixtures/gen_transfer.ml`):
//! the rolling checksum, rsync's sizes, the block information, and every
//! chunk of tokens the source sends, for rsync and for plain contents. Then
//! the chunks decode back to the source file.

use std::io::Cursor;

use gemel_base::util::Error;
use gemel_compat::umarshal;
use gemel_engine::transfer::{self, checksum, rsync};
use md5::{Digest, Md5};

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
