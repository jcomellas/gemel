//! Unison's own Unicode normalisation and case-folding tables, ported as data.
//!
//! Ports Unison `src/unicode.ml`, Unison `src/unicode_tables.ml`.
//!
//! Derived from Unison, Copyright 1999-2020, Benjamin C. Pierce.
//!
//! These functions decide which file names Unison treats as the same file,
//! and their output is hashed into archives, so they must agree with OCaml
//! Unison byte for byte, including on malformed UTF-8. They are *not*
//! standard Unicode normalisation: [`decompose`] and [`compose`] follow the
//! variant of NFD that macOS (HFS+) applies to file names, and [`normalize`]
//! is the case-folding comparison key used by macOS and Windows (Samba).
//! The tables in `unicode/tables.rs` are extracted from Unison's sources by
//! `tools/ocaml-fixtures/gen_unicode.ml`, and the code follows the OCaml
//! closely, quirks included, so that the two can be compared line by line.
//!
//! Strings are byte strings (`&[u8]`), as in OCaml.

use std::cmp::Ordering;

mod tables;
use tables::*;

/// Malformed input (OCaml's `Unicode.Invalid`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Invalid;

impl std::fmt::Display for Invalid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("invalid Unicode string")
    }
}

impl std::error::Error for Invalid {}

type Result<T> = std::result::Result<T, Invalid>;

fn get(s: &[u8], i: usize) -> i64 {
    s[i].into()
}

/// Decodes the character at `i`, returning it and the index after it.
fn decode_char(s: &[u8], i: usize) -> Result<(i64, usize)> {
    let l = s.len();
    if i == l {
        return Err(Invalid);
    }
    let c = get(s, i);
    if c < 0x80 {
        Ok((c, i + 1))
    } else if c < 0xE0 {
        // 80 - 7FF
        if c < 0xc2 || i + 1 >= l {
            return Err(Invalid);
        }
        let c1 = get(s, i + 1);
        if c1 & 0xc0 != 0x80 {
            return Err(Invalid);
        }
        Ok(((c << 6) + c1 - 0x3080, i + 2))
    } else if c < 0xF0 {
        // 800 - FFFF
        if i + 2 >= l {
            return Err(Invalid);
        }
        let c1 = get(s, i + 1);
        let c2 = get(s, i + 2);
        if (c1 | c2) & 0xc0 != 0x80 {
            return Err(Invalid);
        }
        let v = (c << 12) + (c1 << 6) + c2 - 0xe2080;
        if v < 0x800 {
            return Err(Invalid);
        }
        Ok((v, i + 3))
    } else {
        // 10000 - 10FFFF
        decode_4(s, i).map(|v| (v, i + 4))
    }
}

/// Decodes a 4-byte sequence at `i`. Like the OCaml, the continuation check
/// looks at the bitwise OR of the three continuation bytes.
fn decode_4(s: &[u8], i: usize) -> Result<i64> {
    if i + 3 >= s.len() {
        return Err(Invalid);
    }
    let (c, c1, c2, c3) = (get(s, i), get(s, i + 1), get(s, i + 2), get(s, i + 3));
    if (c1 | c2 | c3) & 0xc0 != 0x80 {
        return Err(Invalid);
    }
    let v = (c << 18) + (c1 << 12) + (c2 << 6) + c3 - 0x03c82080;
    if !(0x10000..=0x10ffff).contains(&v) {
        return Err(Invalid);
    }
    Ok(v)
}

/// Encodes `c` at `i` in a buffer of fixed length, returning the index after
/// it. The bounds checks are the OCaml ones, which are looser than they look
/// for 3- and 4-byte characters; callers size their buffers so that they
/// never matter.
fn encode_char(s: &mut [u8], i: usize, c: i64) -> Result<usize> {
    let l = s.len();
    let mut put = |k: usize, v: i64| s[i + k] = v as u8;
    if c < 0x80 {
        if i >= l {
            return Err(Invalid);
        }
        put(0, c);
        Ok(i + 1)
    } else if c < 0x800 {
        if i + 1 >= l {
            return Err(Invalid);
        }
        put(0, (c >> 6) + 0xC0);
        put(1, (c & 0x3f) + 0x80);
        Ok(i + 2)
    } else if c < 0x10000 {
        if i + 1 >= l {
            return Err(Invalid);
        }
        put(0, (c >> 12) + 0xE0);
        put(1, ((c >> 6) & 0x3f) + 0x80);
        put(2, (c & 0x3f) + 0x80);
        Ok(i + 3)
    } else {
        if i + 1 >= l {
            return Err(Invalid);
        }
        put(0, (c >> 18) + 0xF0);
        put(1, ((c >> 12) & 0x3f) + 0x80);
        put(2, ((c >> 6) & 0x3f) + 0x80);
        put(3, (c & 0x3f) + 0x80);
        Ok(i + 4)
    }
}

/// Index of the start of the character before `i`.
fn prev_char(s: &[u8], mut i: usize) -> Result<usize> {
    loop {
        i = i.checked_sub(1).ok_or(Invalid)?;
        if s[i] & 0xc0 != 0x80 {
            return Ok(i);
        }
    }
}

// ---- Canonical ordering of combining characters ----

fn combining_class(c: i64) -> i64 {
    if c > 0xffff {
        return 0;
    }
    let v = i64::from(COMBINING_PROPERTY_BITMAP[(c >> 8) as usize]);
    if v == 0 {
        0
    } else {
        COMBINING_PROPERTY_BITMAP[((v << 8) + (c & 0xff)) as usize].into()
    }
}

fn find_loc(s: &[u8], mut i: usize, p: i64) -> Result<usize> {
    loop {
        if i == 0 {
            return Ok(i);
        }
        let i2 = prev_char(s, i)?;
        let (v, _) = decode_char(s, i2)?;
        if combining_class(v) <= p {
            return Ok(i);
        }
        i = i2;
    }
}

/// Sorts runs of combining characters by combining class, in place
/// (`order` in unicode.ml). The input is the output of `norm` or `decomp`,
/// so characters are not validated again.
fn order(s: &mut [u8]) -> Result<()> {
    let l = s.len();
    let (mut i, mut p) = (0, 0);
    while i < l {
        let c = get(s, i);
        let (v, j) = if c < 0x80 {
            i += 1;
            p = 0;
            continue;
        } else if c < 0xE0 {
            // 80 - 7FF
            if i + 1 >= l {
                return Err(Invalid);
            }
            ((c << 6) + get(s, i + 1) - 0x3080, i + 2)
        } else if c < 0xF0 {
            // 800 - FFFF
            if i + 2 >= l {
                return Err(Invalid);
            }
            (
                (c << 12) + (get(s, i + 1) << 6) + get(s, i + 2) - 0xe2080,
                i + 3,
            )
        } else {
            // 10000 - 10FFFF
            if i + 3 >= l {
                return Err(Invalid);
            }
            i += 4;
            p = 0;
            continue;
        };
        let p2 = combining_class(v);
        if p2 == 0 || p <= p2 {
            p = p2;
        } else {
            // Move the character to the right location.
            let k = find_loc(s, i, p2)?;
            s[k..j].rotate_right(j - i);
        }
        i = j;
    }
    Ok(())
}

// ---- Normalisation and decomposition ----

const HANGUL_SBASE: i64 = 0xAC00;
const HANGUL_LBASE: i64 = 0x1100;
const HANGUL_VBASE: i64 = 0x1161;
const HANGUL_TBASE: i64 = 0x11A7;
const HANGUL_SCOUNT: i64 = 11172;
const HANGUL_LCOUNT: i64 = 19;
const HANGUL_VCOUNT: i64 = 21;
const HANGUL_TCOUNT: i64 = 28;
const HANGUL_NCOUNT: i64 = HANGUL_VCOUNT * HANGUL_TCOUNT;

fn push_char_3(out: &mut Vec<u8>, c: i64) {
    out.extend_from_slice(&[
        ((c >> 12) + 0xE0) as u8,
        (((c >> 6) & 0x3f) + 0x80) as u8,
        ((c & 0x3f) + 0x80) as u8,
    ]);
}

/// One of the two mapping tables: `norm_*` for [`normalize`] and `decomp_*`
/// for [`decompose`].
struct Mapping {
    ascii: &'static [u8; 128],
    repl: &'static [u8],
    prim: &'static [u8; 1024],
    second_high: &'static [u8],
    second_low: &'static [u8],
}

const NORM: Mapping = Mapping {
    ascii: NORM_ASCII,
    repl: NORM_REPL,
    prim: NORM_PRIM,
    second_high: NORM_SECOND_HIGH,
    second_low: NORM_SECOND_LOW,
};

const DECOMP: Mapping = Mapping {
    ascii: DECOMP_ASCII,
    repl: DECOMP_REPL,
    prim: DECOMP_PRIM,
    second_high: DECOMP_SECOND_HIGH,
    second_low: DECOMP_SECOND_LOW,
};

impl Mapping {
    fn replacement(&self, idx: usize, k: u8) -> &'static [u8] {
        let k = ((usize::from(k) - 2) << 8) + usize::from(self.second_low[idx]);
        let n = usize::from(self.repl[k]);
        &self.repl[k + 1..k + 1 + n]
    }

    /// `norm` and `decomp` in unicode.ml, which differ only in their tables.
    fn map(&self, s: &[u8]) -> Result<Vec<u8>> {
        let l = s.len();
        let mut out = Vec::with_capacity(3 * l);
        let mut i = 0;
        while i < l {
            let c = get(s, i);
            if c < 0x80 {
                out.push(self.ascii[c as usize]);
                i += 1;
            } else if c < 0xE0 {
                // 80 - 7FF
                if c < 0xc2 || i + 1 >= l {
                    return Err(Invalid);
                }
                let c1 = get(s, i + 1);
                if c1 & 0xc0 != 0x80 {
                    return Err(Invalid);
                }
                let idx = i64::from(self.prim[(c - 0xc0) as usize]);
                let idx = ((idx << 6) + c1 - 0x80) as usize;
                match self.second_high[idx] {
                    0 => out.extend_from_slice(&s[i..i + 2]),
                    k => out.extend_from_slice(self.replacement(idx, k)),
                }
                i += 2;
            } else if c < 0xF0 {
                // 800 - FFFF
                if i + 2 >= l {
                    return Err(Invalid);
                }
                let c1 = get(s, i + 1);
                if c1 & 0xc0 != 0x80 {
                    return Err(Invalid);
                }
                let idx = (c << 6) + c1 - 0x3880;
                if idx < 0x20 {
                    return Err(Invalid);
                }
                let c2 = get(s, i + 2);
                if c2 & 0xc0 != 0x80 {
                    return Err(Invalid);
                }
                let idx = i64::from(self.prim[idx as usize]);
                let idx = ((idx << 6) + c2 - 0x80) as usize;
                match self.second_high[idx] {
                    0 => out.extend_from_slice(&s[i..i + 3]),
                    1 => {
                        let v = (c << 12) + (c1 << 6) + c2 - (0x000E2080 + HANGUL_SBASE);
                        if v >= HANGUL_SCOUNT {
                            out.extend_from_slice(&s[i..i + 3]);
                        } else {
                            push_char_3(&mut out, v / HANGUL_NCOUNT + HANGUL_LBASE);
                            push_char_3(
                                &mut out,
                                (v % HANGUL_NCOUNT) / HANGUL_TCOUNT + HANGUL_VBASE,
                            );
                            if v % HANGUL_TCOUNT != 0 {
                                push_char_3(&mut out, (v % HANGUL_TCOUNT) + HANGUL_TBASE);
                            }
                        }
                    }
                    k => out.extend_from_slice(self.replacement(idx, k)),
                }
                i += 3;
            } else {
                // 10000 - 10FFFF
                decode_4(s, i)?;
                out.extend_from_slice(&s[i..i + 4]);
                i += 4;
            }
        }
        Ok(out)
    }
}

/// The case-insensitive comparison key: two names are equal under
/// [`case_insensitive_compare`] iff their keys are equal.
///
/// Strings that aren't valid UTF-8 are lowercased as ASCII instead.
pub fn normalize(s: &[u8]) -> Vec<u8> {
    NORM.map(s)
        .and_then(|mut out| order(&mut out).map(|()| out))
        .unwrap_or_else(|Invalid| s.to_ascii_lowercase())
}

/// Decomposes a string the way macOS does for file names.
///
/// Strings that aren't valid UTF-8 are returned unchanged.
pub fn decompose(s: &[u8]) -> Vec<u8> {
    DECOMP
        .map(s)
        .and_then(|mut out| order(&mut out).map(|()| out))
        .unwrap_or_else(|Invalid| s.to_vec())
}

// ---- Comparison ----

fn compare_with(
    s: &[u8],
    s2: &[u8],
    ascii: impl Fn(u8) -> u8,
    full: impl Fn(&[u8]) -> Vec<u8>,
) -> Ordering {
    for (&c, &c2) in s.iter().zip(s2) {
        if c >= 0x80 || c2 >= 0x80 {
            return full(s).cmp(&full(s2));
        }
        match ascii(c).cmp(&ascii(c2)) {
            Ordering::Equal => {}
            other => return other,
        }
    }
    s.len().cmp(&s2.len())
}

/// Compares names as macOS (Darwin) and Windows (Samba) do in their
/// case-insensitive modes: equal iff the [`normalize`]d forms are equal.
pub fn case_insensitive_compare(s: &[u8], s2: &[u8]) -> Ordering {
    compare_with(s, s2, |c| NORM_ASCII[usize::from(c)], normalize)
}

/// Compares names case-sensitively, but up to decomposition.
pub fn case_sensitive_compare(s: &[u8], s2: &[u8]) -> Ordering {
    compare_with(s, s2, |c| c, decompose)
}

// ---- Composition ----

fn bitmap_test(base: i64, bitmap: &[u8], character: i64) -> bool {
    character >= base && character < 0x10000 && {
        let value = i64::from(bitmap[((character >> 8) & 0xFF) as usize]);
        value == 0xFF
            || (value != 0
                && i64::from(bitmap[((value - 1) * 32 + 256 + (character & 0xFF) / 8) as usize])
                    & (1 << (character & 7))
                    != 0)
    }
}

fn unicode_combinable(character: i64) -> bool {
    bitmap_test(0x0300, UNI_CHAR_COMBINING_BITMAP, character)
}

/// Binary search in a table of (key, value) pairs: entries `i..i + n`.
fn find(t: &[u32], i: usize, n: usize, v: i64) -> i64 {
    let key = |k: usize| i64::from(t[2 * k]);
    let mut j = i + n;
    if v < key(i) || v > key(j - 1) {
        return 0;
    }
    let mut i = i;
    while i + 1 != j {
        let k = (i + j) / 2;
        if v < key(k) {
            j = k;
        } else {
            i = k;
        }
    }
    if key(i) == v { t[i * 2 + 1].into() } else { 0 }
}

/// The precomposed character for `v` followed by `v2`, or 0.
fn combine(v: i64, v2: i64) -> i64 {
    if (HANGUL_VBASE..HANGUL_TBASE + HANGUL_TCOUNT).contains(&v2) {
        if v2 < HANGUL_VBASE + HANGUL_VCOUNT
            && (HANGUL_LBASE..HANGUL_LBASE + HANGUL_LCOUNT).contains(&v)
        {
            HANGUL_SBASE
                + ((v - HANGUL_LBASE) * (HANGUL_VCOUNT * HANGUL_TCOUNT))
                + ((v2 - HANGUL_VBASE) * HANGUL_TCOUNT)
        } else if v2 > HANGUL_TBASE && (HANGUL_SBASE..HANGUL_SBASE + HANGUL_SCOUNT).contains(&v) {
            if (v - HANGUL_SBASE) % HANGUL_TCOUNT != 0 {
                0
            } else {
                v + v2 - HANGUL_TBASE
            }
        } else {
            0
        }
    } else {
        let source = &UNI_CHAR_PRECOMP_SOURCE_TABLE;
        let k = find(source, 0, source.len() / 2, v2);
        if k == 0 {
            0
        } else {
            find(
                &UNI_CHAR_BMP_PRECOMP_DESTINATION_TABLE,
                (k & 0xFFFF) as usize,
                (k >> 16) as usize,
                v,
            )
        }
    }
}

fn compose_rec(s: &[u8], mut i: usize, mut v: i64) -> (i64, usize) {
    while let Ok((v2, j)) = decode_char(s, i) {
        if !unicode_combinable(v2) {
            break;
        }
        match combine(v, v2) {
            0 => break,
            v3 => {
                v = v3;
                i = j;
            }
        }
    }
    (v, i)
}

/// `scan` and `cont` in the composition part of unicode.ml. Works in place:
/// `s[..i1]` is output, `s[i1..i2]` is a gap left by composed characters,
/// and `s[i2..]` is input not yet moved down over the gap.
fn compose_in_place(s: &mut [u8]) -> Result<usize> {
    let l = s.len();
    let (mut i1, mut i2) = (0, 0);
    let mut i = 0;
    while i < l {
        let (v2, j) = decode_char(s, i)?;
        if !unicode_combinable(v2) {
            i = j;
            continue;
        }
        let prev = prev_char(s, i)?;
        let (v, _) = decode_char(s, prev)?;
        let v3 = combine(v, v2);
        if v3 == 0 {
            i = j;
            continue;
        }
        // `prev` would fall inside the gap if a mark left there by
        // `compose_rec` combined with the next mark; OCaml would then call
        // `Bytes.blit` with a negative length and die. With Unison's tables
        // this can't happen (checked by brute force: the marks that combine
        // with a following mark, such as U+09C7, never combine with a
        // preceding character, so `compose_rec` never consumes them), but
        // leave the string uncomposed rather than panic if it ever did.
        if prev < i2 {
            return Err(Invalid);
        }
        s.copy_within(i2..prev, i1);
        i1 += prev - i2;
        let (v3, next) = compose_rec(s, j, v3);
        i1 = encode_char(s, i1, v3)?;
        i2 = next;
        i = next;
    }
    s.copy_within(i2..l, i1);
    Ok(i1 + l - i2)
}

/// Recomposes a string decomposed by macOS.
///
/// Strings that aren't valid UTF-8 are returned unchanged.
pub fn compose(s: &[u8]) -> Vec<u8> {
    let mut buf = s.to_vec();
    match compose_in_place(&mut buf) {
        Ok(len) => {
            buf.truncate(len);
            buf
        }
        Err(Invalid) => s.to_vec(),
    }
}

// ---- UTF-16 ----

fn push_2(out: &mut Vec<u8>, v: i64) {
    out.extend_from_slice(&[(v & 0xff) as u8, (v >> 8) as u8]);
}

fn get_2(s: &[u8], i: usize) -> i64 {
    (get(s, i + 1) << 8) + get(s, i)
}

/// UTF-8 to null-terminated UTF-16LE. `map_ascii` handles characters below
/// 0x80, given the input and the character's index.
fn to_utf_16_with(s: &[u8], map_ascii: impl Fn(&[u8], usize) -> i64) -> Result<Vec<u8>> {
    let l = s.len();
    let mut out = Vec::with_capacity(2 * l + 2);
    let mut i = 0;
    while i < l {
        let c = get(s, i);
        if c < 0x80 {
            push_2(&mut out, map_ascii(s, i));
            i += 1;
        } else if c >= 0xF0 {
            // 10000 - 10FFFF
            let v = decode_4(s, i)? - 0x10000;
            push_2(&mut out, (v >> 10) + 0xD800);
            push_2(&mut out, (v & 0x3FF) + 0xDC00);
            i += 4;
        } else {
            let (v, j) = decode_char(s, i)?;
            push_2(&mut out, v);
            i = j;
        }
    }
    out.extend_from_slice(&[0, 0]);
    Ok(out)
}

/// Converts UTF-8 to null-terminated UTF-16LE.
pub fn to_utf_16(s: &[u8]) -> Result<Vec<u8>> {
    to_utf_16_with(s, get)
}

/// Converts a UTF-8 file name to null-terminated UTF-16LE, mapping
/// characters NTFS forbids to the private use area as Services for
/// Macintosh does (including a trailing space or dot in a path component).
pub fn to_utf_16_filename(s: &[u8]) -> Result<Vec<u8>> {
    to_utf_16_with(s, |s, i| {
        let c = get(s, i);
        let end_of_name = i + 1 == s.len() || s[i + 1] == b'/';
        if c == 0x20 && end_of_name {
            0xf028
        } else if c == 0x2e && end_of_name {
            0xf029
        } else {
            SFM_ENCODE[c as usize].into()
        }
    })
}

/// UTF-16LE to UTF-8, stopping at the first null. `map_bmp` handles code
/// units above the surrogates, given the input and the unit's index.
fn from_utf_16_with(s: &[u8], map_bmp: impl Fn(&[u8], usize, i64) -> i64) -> Result<Vec<u8>> {
    let l = s.len();
    let mut out = vec![0; 3 * l / 2];
    let (mut i, mut j) = (0, 0);
    while i + 2 <= l {
        let v = get_2(s, i);
        if v == 0 {
            break;
        }
        let (c, width) = if v < 0xD800 {
            (v, 2)
        } else if v > 0xDFFF {
            (map_bmp(s, i, v), 2)
        } else if v >= 0xdc00 || i + 4 > l {
            // A lone surrogate becomes an ill-formed UTF-8 sequence rather
            // than an error.
            (v, 2)
        } else {
            let v2 = get_2(s, i + 2);
            if !(0xDC00..=0xDFFF).contains(&v2) {
                (v, 2)
            } else {
                (((v - 0xD800) << 10) + (v2 - 0xDC00) + 0x10000, 4)
            }
        };
        j = encode_char(&mut out, j, c)?;
        i += width;
    }
    if i + 2 > l && i < l {
        // Odd number of bytes.
        return Err(Invalid);
    }
    out.truncate(j);
    Ok(out)
}

/// Converts UTF-16LE (up to the first null) to UTF-8. Lone surrogates are
/// encoded as ill-formed 3-byte sequences, so this only fails on an odd
/// number of bytes.
pub fn from_utf_16(s: &[u8]) -> Result<Vec<u8>> {
    from_utf_16_with(s, |_, _, v| v)
}

/// The inverse of [`to_utf_16_filename`].
pub fn from_utf_16_filename(s: &[u8]) -> Result<Vec<u8>> {
    from_utf_16_with(s, |s, i, v| {
        if v > 0xf000 && v <= 0xf029 {
            let l = s.len();
            let end_of_name = i + 2 == l || (i + 4 <= l && s[i + 2] == b'/' && s[i + 3] == 0);
            if v == 0xf028 && end_of_name {
                0x20
            } else if v == 0xf029 && end_of_name {
                0x2e
            } else {
                SFM_DECODE[(v - 0xf000) as usize].into()
            }
        } else {
            v
        }
    })
}

// ---- Validation and protection ----

/// Whether `s` is well-formed UTF-8 without NUL, surrogates, U+FFFE or
/// U+FFFF.
pub fn check_utf_8(s: &[u8]) -> bool {
    let l = s.len();
    let mut i = 0;
    while i < l {
        let c = get(s, i);
        if c < 0x80 {
            if c == 0 {
                return false;
            }
            i += 1;
        } else if c < 0xE0 {
            // 80 - 7FF
            if c < 0xc2 || i + 1 >= l || get(s, i + 1) & 0xc0 != 0x80 {
                return false;
            }
            i += 2;
        } else if c < 0xF0 {
            // 800 - FFFF
            if i + 2 >= l {
                return false;
            }
            let (c1, c2) = (get(s, i + 1), get(s, i + 2));
            if (c1 | c2) & 0xc0 != 0x80 {
                return false;
            }
            let v = (c << 12) + (c1 << 6) + c2 - 0xe2080;
            if !(v >= 0x800 && (v < 0xd800 || (v > 0xdfff && v != 0xfffe && v != 0xffff))) {
                return false;
            }
            i += 3;
        } else {
            // 10000 - 10FFFF
            if decode_4(s, i).is_err() {
                return false;
            }
            i += 4;
        }
    }
    true
}

/// Byte ranges of each well-formed UTF-8 sequence (`wf_utf8`).
const WF_UTF8: &[&[(u8, u8)]] = &[
    &[(0x01, 0x7F)],
    &[(0xC2, 0xDF), (0x80, 0xBF)],
    &[(0xE0, 0xE0), (0xA0, 0xBF), (0x80, 0xBF)],
    &[(0xE1, 0xEC), (0x80, 0xBF), (0x80, 0xBF)],
    &[(0xED, 0xED), (0x80, 0x9F), (0x80, 0xBF)],
    &[(0xEE, 0xEF), (0x80, 0xBF), (0x80, 0xBF)],
    &[(0xF0, 0xF0), (0x90, 0xBF), (0x80, 0xBF), (0x80, 0xBF)],
    &[(0xF1, 0xF3), (0x80, 0xBF), (0x80, 0xBF), (0x80, 0xBF)],
    &[(0xF4, 0xF4), (0x80, 0x8F), (0x80, 0xBF), (0x80, 0xBF)],
];

/// Length of the well-formed sequence at the start of `s`, if any.
fn accept(s: &[u8]) -> Option<usize> {
    WF_UTF8.iter().find_map(|seq| {
        (seq.len() <= s.len() && seq.iter().zip(s).all(|(&(a, b), &c)| (a..=b).contains(&c)))
            .then_some(seq.len())
    })
}

/// Converts to UTF-8, keeping well-formed UTF-8 sequences and treating other
/// bytes as ISO 8859-1. NUL becomes a space.
pub fn protect(s: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(2 * s.len());
    let mut i = 0;
    while i < s.len() {
        if let Some(n) = accept(&s[i..]) {
            out.extend_from_slice(&s[i..i + n]);
            i += n;
            continue;
        }
        match s[i] {
            0 => out.push(b' '),
            c if c < 0x80 => out.push(c),
            c => out.extend_from_slice(&[(c >> 6) + 0xC0, (c & 0x3f) + 0x80]),
        }
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decompose_and_compose_round_trip_latin() {
        let composed = "Ångström café".as_bytes();
        let decomposed = decompose(composed);
        assert_eq!(decomposed, "A\u{30a}ngstro\u{308}m cafe\u{301}".as_bytes());
        assert_eq!(compose(&decomposed), composed);
    }

    #[test]
    fn hangul_t_base_is_not_combinable() {
        // So `combine` never sees U+11A7, and `v2 > HANGUL_TBASE` there is
        // only a guard.
        assert!(!unicode_combinable(HANGUL_TBASE));
        assert!(unicode_combinable(HANGUL_TBASE + 1));
    }

    #[test]
    fn case_insensitive_compare_folds_case_and_decomposition() {
        assert_eq!(
            case_insensitive_compare("CAFÉ".as_bytes(), "cafe\u{301}".as_bytes()),
            Ordering::Equal
        );
        assert_eq!(case_sensitive_compare(b"a", b"B"), Ordering::Greater);
    }
}
