//! Checks gemel-compat against golden vectors produced by OCaml Unison
//! (`tests/fixtures/*.txt`, see `scripts/gen-ocaml-fixtures.sh`).

use std::collections::BTreeMap;

use gemel_compat::ocaml_hash::{self, Value};
use gemel_compat::umarshal::{self, OCAML_MAX_INT, OCAML_MIN_INT, Reader, Result, Writer};

fn parse(text: &str) -> BTreeMap<&str, &str> {
    text.lines()
        .filter(|l| !l.starts_with('#') && !l.is_empty())
        .map(|l| l.split_once(' ').expect("`<name> <value>` line"))
        .collect()
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex"))
        .collect()
}

/// Every case in a fixture file must be checked, and every checked case must
/// exist, so that cases can't silently drop out on either side.
struct Cases<'a> {
    expected: BTreeMap<&'a str, &'a str>,
    seen: Vec<String>,
}

impl<'a> Cases<'a> {
    fn new(text: &'a str) -> Self {
        Self {
            expected: parse(text),
            seen: Vec::new(),
        }
    }

    fn take(&mut self, name: &str) -> &'a str {
        self.seen.push(name.to_owned());
        self.expected
            .get(name)
            .unwrap_or_else(|| panic!("no fixture named {name}"))
    }

    fn finish(&self) {
        let unchecked: Vec<_> = self
            .expected
            .keys()
            .filter(|k| !self.seen.iter().any(|s| s == *k))
            .collect();
        assert!(unchecked.is_empty(), "fixtures not checked: {unchecked:?}");
    }
}

// --- Umarshal ---------------------------------------------------------------

#[derive(Debug, PartialEq)]
enum AttrValue {
    String(Vec<u8>),
    Hash(Vec<u8>),
    Loaded(Vec<u8>, Vec<u8>),
}

type Xattrs = Option<(Vec<(Vec<u8>, AttrValue)>, i64)>;

fn write_attrvalue(w: &mut Writer, v: &AttrValue) {
    match v {
        AttrValue::String(s) => {
            w.tag(0);
            w.string(s);
        }
        AttrValue::Hash(s) => {
            w.tag(1);
            w.string(s);
        }
        AttrValue::Loaded(full, hash) => {
            w.tag(2);
            w.string(full);
            w.string(hash);
        }
    }
}

fn read_attrvalue(r: &mut Reader<'_>) -> Result<AttrValue> {
    Ok(match r.tag(3)? {
        0 => AttrValue::String(r.string()?),
        1 => AttrValue::Hash(r.string()?),
        _ => AttrValue::Loaded(r.string()?, r.string()?),
    })
}

fn write_xattrs(w: &mut Writer, x: &Xattrs) {
    w.option(x.as_ref(), |w, (attrs, size)| {
        w.list(attrs, |w, (name, value)| {
            w.string(name);
            write_attrvalue(w, value);
        });
        w.int64(*size);
    });
}

fn read_xattrs(r: &mut Reader<'_>) -> Result<Xattrs> {
    r.option(|r| {
        Ok((
            r.list(|r| Ok((r.string()?, read_attrvalue(r)?)))?,
            r.int64()?,
        ))
    })
}

fn xattrs_sample() -> Xattrs {
    Some((
        vec![
            (b"user.a".to_vec(), AttrValue::String(b"v".to_vec())),
            (b"user.b".to_vec(), AttrValue::Hash(vec![1, 2, 3])),
            (
                b"com.apple.FinderInfo".to_vec(),
                AttrValue::Loaded(b"full value".to_vec(), b"hash".to_vec()),
            ),
        ],
        1234,
    ))
}

const NON_UTF8: &[u8] = b"\xff\xfe\x00\x80caf\xc3\xa9";

fn long_string() -> Vec<u8> {
    (0..300).map(|i| (i % 256) as u8).collect()
}

/// Checks that `write` produces the fixture's bytes and that `read` decodes
/// them back to `value`.
fn check<T: PartialEq + std::fmt::Debug>(
    cases: &mut Cases<'_>,
    name: &str,
    value: T,
    write: impl Fn(&mut Writer, &T),
    read: impl Fn(&mut Reader<'_>) -> Result<T>,
) {
    let expected = unhex(cases.take(name));
    assert_eq!(
        umarshal::to_bytes(|w| write(w, &value)),
        expected,
        "encoding of {name}"
    );
    assert_eq!(
        umarshal::from_bytes(&expected, read).as_ref(),
        Ok(&value),
        "decoding of {name}"
    );
}

#[test]
fn umarshal_matches_ocaml() {
    let c = &mut Cases::new(include_str!("fixtures/umarshal.txt"));
    check(c, "unit", (), |w, ()| w.unit(), |r| r.unit());
    check(c, "bool_false", false, |w, b| w.bool(*b), |r| r.bool());
    check(c, "bool_true", true, |w, b| w.bool(*b), |r| r.bool());
    for (name, n) in [
        ("0", 0),
        ("1", 1),
        ("m1", -1),
        ("127", 127),
        ("128", 128),
        ("m128", -128),
        ("m129", -129),
        ("32767", 32767),
        ("32768", 32768),
        ("m32768", -32768),
        ("m32769", -32769),
        ("2p30", 1 << 30),
        ("2p31m1", (1 << 31) - 1),
        ("2p31", 1 << 31),
        ("m2p31", -(1 << 31)),
        ("m2p31m1", -(1 << 31) - 1),
        ("max", OCAML_MAX_INT),
        ("min", OCAML_MIN_INT),
    ] {
        check(c, &format!("int_{name}"), n, |w, n| w.int(*n), |r| r.int());
    }
    for (name, n) in [
        ("0", 0),
        ("2p31", 1 << 31),
        ("max", i64::MAX),
        ("min", i64::MIN),
        ("m1", -1),
    ] {
        check(
            c,
            &format!("int64_{name}"),
            n,
            |w, n| w.int64(*n),
            |r| r.int64(),
        );
    }
    // Compare floats by bits so that -0.0 and NaN payloads are checked too.
    for (name, x) in [
        ("0", 0.0),
        ("m0", -0.0),
        ("1_5", 1.5),
        ("inf", f64::INFINITY),
        ("nan", f64::from_bits(0x7FF8_0000_0000_0001)),
    ] {
        check(
            c,
            &format!("float_{name}"),
            x.to_bits(),
            |w, b| w.float(f64::from_bits(*b)),
            |r| r.float().map(f64::to_bits),
        );
    }
    let string = |w: &mut Writer, s: &Vec<u8>| w.string(s);
    check(c, "string_empty", vec![], string, |r| r.string());
    check(c, "string_a", b"a".to_vec(), string, |r| r.string());
    check(c, "string_non_utf8", NON_UTF8.to_vec(), string, |r| {
        r.string()
    });
    check(c, "string_300", long_string(), string, |r| r.string());
    check(
        c,
        "bytearray_hi",
        b"hi".to_vec(),
        |w, b| w.bytearray(b),
        |r| r.bytearray(),
    );
    check(
        c,
        "int32bigarray",
        vec![0, 1, -1, i32::MAX, i32::MIN],
        |w, a| w.int32bigarray(a),
        |r| r.int32bigarray(),
    );
    let opt_int = |w: &mut Writer, o: &Option<i64>| w.option(o.as_ref(), |w, n| w.int(*n));
    check(c, "option_none", None, opt_int, |r| r.option(|r| r.int()));
    check(c, "option_some_int", Some(5), opt_int, |r| {
        r.option(|r| r.int())
    });
    let opt_opt = |w: &mut Writer, o: &Option<Option<Vec<u8>>>| {
        w.option(o.as_ref(), |w, o| w.option(o.as_ref(), |w, s| w.string(s)))
    };
    let read_opt_opt = |r: &mut Reader<'_>| r.option(|r| r.option(|r| r.string()));
    check(c, "option_some_none", Some(None), opt_opt, read_opt_opt);
    check(
        c,
        "option_some_some",
        Some(Some(b"x".to_vec())),
        opt_opt,
        read_opt_opt,
    );
    let ints = |w: &mut Writer, l: &Vec<i64>| w.list(l, |w, n| w.int(*n));
    check(c, "list_empty", vec![], ints, |r| r.list(|r| r.int()));
    check(c, "list_ints", vec![1, 2, 300], ints, |r| {
        r.list(|r| r.int())
    });
    check(
        c,
        "list_options",
        vec![Some(b"a".to_vec()), None, Some(vec![])],
        |w, l| w.list(l, |w, o| w.option(o.as_ref(), |w, s| w.string(s))),
        |r| r.list(|r| r.option(|r| r.string())),
    );
    check(
        c,
        "prod3",
        (b"a".to_vec(), -2, true),
        |w, (s, n, b)| {
            w.string(s);
            w.int(*n);
            w.bool(*b);
        },
        |r| Ok((r.string()?, r.int()?, r.bool()?)),
    );
    check(
        c,
        "attrvalue_loaded",
        AttrValue::Loaded(b"full".to_vec(), b"h".to_vec()),
        write_attrvalue,
        read_attrvalue,
    );
    check(c, "xattr_none", None, write_xattrs, read_xattrs);
    check(
        c,
        "xattr_empty",
        Some((vec![], 0)),
        write_xattrs,
        read_xattrs,
    );
    check(c, "xattr_some", xattrs_sample(), write_xattrs, read_xattrs);
    c.finish();
}

// --- Legacy hash ------------------------------------------------------------

fn s(bytes: &[u8]) -> Value {
    Value::string(bytes)
}

fn xattrs_value() -> Value {
    // `(string * attrvalue) list * int64` option, with `Loaded of (string *
    // string)` holding a pointer to a separate tuple.
    let attrs = Value::list([
        Value::tuple([s(b"user.a"), Value::Block(0, vec![s(b"v")])]),
        Value::tuple([s(b"user.b"), Value::Block(1, vec![s(&[1, 2, 3])])]),
        Value::tuple([
            s(b"com.apple.FinderInfo"),
            Value::Block(2, vec![Value::tuple([s(b"full value"), s(b"hash")])]),
        ]),
    ]);
    Value::some(Value::tuple([attrs, Value::Int64(1234)]))
}

// `type nest = L of int | N of nest` in the generator.
fn nest(depth: usize) -> Value {
    (0..depth).fold(Value::Block(0, vec![Value::Int(7)]), |inner, _| {
        Value::Block(1, vec![inner])
    })
}

#[test]
fn ocaml_hash_matches_ocaml() {
    let c = &mut Cases::new(include_str!("fixtures/ocaml_hash.txt"));
    let mut check = |name: &str, actual: i64| {
        let expected: i64 = c.take(name).parse().expect("decimal hash");
        assert_eq!(actual, expected, "{name}");
    };
    for (name, n) in [
        ("0", 0),
        ("1", 1),
        ("m1", -1),
        ("2p30", 1 << 30),
        ("max", OCAML_MAX_INT),
        ("min", OCAML_MIN_INT),
    ] {
        check(&format!("int_{name}"), ocaml_hash::hash(&Value::Int(n)));
        check(&format!("int_{name}"), ocaml_hash::hash_int(n));
    }
    for (name, bytes) in [
        ("empty", &b""[..]),
        ("a", b"a"),
        ("hello", b"hello"),
        ("non_utf8", NON_UTF8),
        ("300", &long_string()),
    ] {
        check(&format!("string_{name}"), ocaml_hash::hash(&s(bytes)));
        check(&format!("string_{name}"), ocaml_hash::hash_string(bytes));
    }
    for (name, n) in [("0", 0), ("m1", -1), ("2p32", 1 << 32), ("min", i64::MIN)] {
        check(&format!("int64_{name}"), ocaml_hash::hash(&Value::Int64(n)));
    }
    check("float_1_5", ocaml_hash::hash(&Value::Double(1.5)));
    check("option_none", ocaml_hash::hash(&Value::NONE));
    check(
        "option_some",
        ocaml_hash::hash(&Value::some(s(b"TEXTMooV"))),
    );
    check(
        "fullfingerprint",
        ocaml_hash::hash(&Value::tuple([s(b"0123456789abcdef"), s(b"")])),
    );
    check("xattr_none", ocaml_hash::hash(&Value::NONE));
    check(
        "xattr_empty",
        ocaml_hash::hash(&Value::some(Value::tuple([Value::NIL, Value::Int64(0)]))),
    );
    check("xattr_some", ocaml_hash::hash(&xattrs_value()));
    check(
        "tuple_mixed",
        ocaml_hash::hash(&Value::tuple([
            Value::Int(1),
            s(b"a"),
            Value::some(Value::Int64(2)),
        ])),
    );
    check(
        "list_ints_20",
        ocaml_hash::hash(&Value::list((0..20).map(Value::Int))),
    );
    check(
        "list_strings_20",
        ocaml_hash::hash(&Value::list((0..20).map(|i| s(i.to_string().as_bytes())))),
    );
    check(
        "list_ints_3",
        ocaml_hash::hash(&Value::list([3, 1, 2].map(Value::Int))),
    );
    check(
        "list_strings_4",
        ocaml_hash::hash(&Value::list([s(b"a"), s(b"bc"), s(b""), s(b"d")])),
    );
    for depth in [3, 9, 10, 120] {
        check(&format!("nest_{depth}"), ocaml_hash::hash(&nest(depth)));
    }
    check("hash2_0_0", ocaml_hash::hash2(0, 0));
    check("hash2_1_2", ocaml_hash::hash2(1, 2));
    check("hash2_m1_max", ocaml_hash::hash2(-1, OCAML_MAX_INT));
    check("hash2_min_m1", ocaml_hash::hash2(OCAML_MIN_INT, -1));
    for (name, n) in [
        ("0", 0),
        ("1234", 1234),
        ("2p40", 1 << 40),
        ("max", i64::MAX),
    ] {
        check(&format!("filesize_{name}"), ocaml_hash::hash_filesize(n));
    }
    c.finish();
}
