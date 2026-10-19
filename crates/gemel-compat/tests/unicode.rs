//! Checks `gemel_compat::unicode` against golden vectors produced by OCaml
//! Unison (`tests/fixtures/unicode_*.txt`, see `scripts/gen-ocaml-fixtures.sh`).

use std::cmp::Ordering;

use gemel_compat::unicode::{self, Invalid};
use md5::{Digest, Md5};

fn unhex(s: &str) -> Vec<u8> {
    if s == "-" {
        return vec![];
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex"))
        .collect()
}

fn hex_or_invalid(r: Result<Vec<u8>, Invalid>) -> String {
    match r {
        Err(Invalid) => "!".into(),
        Ok(v) if v.is_empty() => "-".into(),
        Ok(v) => v.iter().map(|b| format!("{b:02x}")).collect(),
    }
}

fn hex(v: Vec<u8>) -> String {
    hex_or_invalid(Ok(v))
}

fn sign(o: Ordering) -> &'static str {
    match o {
        Ordering::Less => "-1",
        Ordering::Equal => "0",
        Ordering::Greater => "1",
    }
}

fn lines(text: &str) -> impl Iterator<Item = Vec<&str>> {
    text.lines()
        .filter(|l| !l.starts_with('#') && !l.is_empty())
        .map(|l| l.split(' ').collect())
}

/// UTF-8 bytes for any value up to 0x10FFFF, surrogates included (as in the
/// generator; `char` can't hold surrogates).
fn utf8(c: u32) -> Vec<u8> {
    if c < 0x80 {
        vec![c as u8]
    } else if c < 0x800 {
        vec![0xC0 | (c >> 6) as u8, 0x80 | (c & 0x3F) as u8]
    } else if c < 0x10000 {
        vec![
            0xE0 | (c >> 12) as u8,
            0x80 | ((c >> 6) & 0x3F) as u8,
            0x80 | (c & 0x3F) as u8,
        ]
    } else {
        vec![
            0xF0 | (c >> 18) as u8,
            0x80 | ((c >> 12) & 0x3F) as u8,
            0x80 | ((c >> 6) & 0x3F) as u8,
            0x80 | (c & 0x3F) as u8,
        ]
    }
}

/// UTF-16LE code units for a code point; a surrogate is one unit.
fn utf16(c: u32) -> Vec<u8> {
    if c < 0x10000 {
        (c as u16).to_le_bytes().to_vec()
    } else {
        let v = c - 0x10000;
        let mut out = ((0xD800 + (v >> 10)) as u16).to_le_bytes().to_vec();
        out.extend_from_slice(&((0xDC00 + (v & 0x3FF)) as u16).to_le_bytes());
        out
    }
}

type Op = fn(u32) -> Result<Vec<u8>, Invalid>;

fn op(name: &str) -> Op {
    match name {
        "normalize" => |c| Ok(unicode::normalize(&utf8(c))),
        "decompose" => |c| Ok(unicode::decompose(&utf8(c))),
        "compose" => |c| Ok(unicode::compose(&utf8(c))),
        "compose_decompose" => |c| Ok(unicode::compose(&unicode::decompose(&utf8(c)))),
        "check_utf_8" => |c| {
            let ok = unicode::check_utf_8(&utf8(c));
            Ok(if ok { b"1".to_vec() } else { b"0".to_vec() })
        },
        "protect" => |c| Ok(unicode::protect(&utf8(c))),
        "to_utf_16" => |c| unicode::to_utf_16(&utf8(c)),
        "to_utf_16_filename" => |c| unicode::to_utf_16_filename(&utf8(c)),
        "from_utf_16" => |c| unicode::from_utf_16(&utf16(c)),
        "from_utf_16_filename" => |c| unicode::from_utf_16_filename(&utf16(c)),
        _ => panic!("unknown operation {name}"),
    }
}

/// Every code point, every operation, in blocks of 0x1000.
#[test]
fn every_code_point_matches_ocaml() {
    let mut mismatches = Vec::new();
    let mut checked = 0;
    for fields in lines(include_str!("fixtures/unicode_codepoints.txt")) {
        let [name, block, expected] = fields[..] else {
            panic!("bad line {fields:?}");
        };
        let f = op(name);
        let start = u32::from_str_radix(block, 16).expect("block");
        let mut md5 = Md5::new();
        for c in start..start + 0x1000 {
            match f(c) {
                Ok(out) => {
                    md5.update(out.len().to_string());
                    md5.update(b":");
                    md5.update(&out);
                }
                Err(Invalid) => md5.update(b"!"),
            }
        }
        let actual: String = md5.finalize().iter().map(|b| format!("{b:02x}")).collect();
        if actual != expected {
            mismatches.push(format!("{name} {block}"));
        }
        checked += 1;
    }
    assert_eq!(checked, 10 * 0x110, "operations x blocks");
    assert!(mismatches.is_empty(), "blocks that differ: {mismatches:?}");
}

#[test]
fn string_corpus_matches_ocaml() {
    let (mut s_lines, mut w_lines, mut c_lines) = (0, 0, 0);
    for fields in lines(include_str!("fixtures/unicode_strings.txt")) {
        match fields[0] {
            "s" => {
                let input = unhex(fields[1]);
                let actual = [
                    hex(unicode::normalize(&input)),
                    hex(unicode::decompose(&input)),
                    hex(unicode::compose(&input)),
                    (if unicode::check_utf_8(&input) {
                        "1"
                    } else {
                        "0"
                    })
                    .into(),
                    hex(unicode::protect(&input)),
                    hex_or_invalid(unicode::to_utf_16(&input)),
                    hex_or_invalid(unicode::to_utf_16_filename(&input)),
                ];
                assert_eq!(actual[..], fields[2..], "input {}", fields[1]);
                s_lines += 1;
            }
            "w" => {
                let input = unhex(fields[1]);
                let actual = [
                    hex_or_invalid(unicode::from_utf_16(&input)),
                    hex_or_invalid(unicode::from_utf_16_filename(&input)),
                ];
                assert_eq!(actual[..], fields[2..], "input {}", fields[1]);
                w_lines += 1;
            }
            "c" => {
                // Columns 3 and 4 are the Unicode case modes; the others are
                // checked by gemel-base's `case` tests.
                let (a, b) = (unhex(fields[1]), unhex(fields[2]));
                let actual = [
                    sign(unicode::case_sensitive_compare(&a, &b)),
                    sign(unicode::case_insensitive_compare(&a, &b)),
                ];
                assert_eq!(actual[..], fields[5..7], "{} vs {}", fields[1], fields[2]);
                c_lines += 1;
            }
            "h" => {}
            other => panic!("unknown line kind {other}"),
        }
    }
    assert!(s_lines >= 600 && w_lines >= 300 && c_lines >= 2000);
}
