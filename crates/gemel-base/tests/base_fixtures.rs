//! Checks clroot and the util helpers against golden vectors produced by
//! OCaml Unison (gemel-compat's `tests/fixtures/base.txt`, see
//! `scripts/gen-ocaml-fixtures.sh`).

use gemel_base::clroot::{self, Clroot, ClrootError};
use gemel_base::util;

const FIXTURE: &str = include_str!("../../gemel-compat/tests/fixtures/base.txt");

fn unhex(s: &str) -> String {
    if s == "-" {
        return String::new();
    }
    let bytes: Vec<u8> = (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex"))
        .collect();
    String::from_utf8(bytes).expect("UTF-8 input")
}

fn hex(s: &str) -> String {
    if s.is_empty() {
        "-".into()
    } else {
        s.bytes().map(|b| format!("{b:02x}")).collect()
    }
}

fn opt(o: &Option<String>) -> String {
    o.as_ref().map_or("~".into(), |s| format!("={s}"))
}

/// `show_root` in gen_base.ml.
fn show(r: &Clroot) -> String {
    match r {
        Clroot::ConnectLocal(p) => format!("L\t{}", opt(p)),
        Clroot::ConnectByShell {
            shell,
            host,
            user,
            port,
            path,
        } => format!(
            "H\t{shell}\t{host}\t{}\t{}\t{}",
            opt(user),
            opt(port),
            opt(path)
        ),
        Clroot::ConnectBySocket { host, port, path } => format!("S\t{host}\t{port}\t{}", opt(path)),
    }
}

fn show_err(e: &ClrootError) -> String {
    let kind = match e {
        ClrootError::Fatal(_) => "Fatal",
        ClrootError::Transient(_) => "Transient",
        ClrootError::IllegalValue(_) => "IllegalValue",
        ClrootError::InvalidArgument(_) => "InvalidArgument",
    };
    format!("E\t{kind}\t{e}")
}

fn show_result(r: Result<Clroot, ClrootError>) -> String {
    r.map_or_else(|e| show_err(&e), |r| show(&r))
}

/// Parses the `show_root` form back, for the `fix` lines' inputs.
fn parse_shown(fields: &[&str]) -> Clroot {
    let opt = |s: &str| s.strip_prefix('=').map(str::to_owned);
    match fields {
        ["L", p] => Clroot::ConnectLocal(opt(p)),
        ["H", shell, host, user, port, path] => Clroot::ConnectByShell {
            shell: shell.to_string(),
            host: host.to_string(),
            user: opt(user),
            port: opt(port),
            path: opt(path),
        },
        ["S", host, port, path] => Clroot::ConnectBySocket {
            host: host.to_string(),
            port: port.to_string(),
            path: opt(path),
        },
        other => panic!("bad root {other:?}"),
    }
}

#[test]
fn clroot_matches_ocaml() {
    let mut checked = 0;
    for line in FIXTURE.lines().filter(|l| !l.starts_with('#')) {
        let (kind, rest) = line.split_once('\t').unwrap();
        match kind {
            "root" => {
                let (input, expected) = rest.split_once('\t').unwrap();
                let input = unhex(input);
                let actual = match clroot::parse_root(&input) {
                    Ok(r) => format!(
                        "{}\t|\t{r}\t|\t{}",
                        show(&r),
                        show_result(clroot::fix_host(r.clone()))
                    ),
                    Err(e) => show_err(&e),
                };
                assert_eq!(actual, expected, "parseRoot {input:?}");
                checked += 1;
            }
            "fix" => {
                let fields: Vec<&str> = rest.split('\t').collect();
                let n = match fields[0] {
                    "L" => 2,
                    "H" => 6,
                    _ => 4,
                };
                let input = parse_shown(&fields[..n]);
                let expected = fields[n..].join("\t");
                assert_eq!(
                    show_result(clroot::fix_host(input.clone())),
                    expected,
                    "fixHost {input:?}"
                );
                checked += 1;
            }
            _ => {}
        }
    }
    assert!(checked >= 70, "{checked}");
}

#[test]
fn util_helpers_match_ocaml() {
    let mut checked = 0;
    for line in FIXTURE.lines().filter(|l| !l.starts_with('#')) {
        let fields: Vec<&str> = line.split('\t').collect();
        let input = || unhex(fields[1]);
        match fields[0] {
            "int" => {
                let actual = util::int_of_string(&input()).map_or("!".into(), |n| n.to_string());
                assert_eq!(actual, fields[2], "int_of_string {:?}", input());
            }
            "words" => {
                let actual: Vec<String> = util::split_into_words(&input(), ' ')
                    .iter()
                    .map(|w| hex(w))
                    .collect();
                let expected = if fields[2].is_empty() {
                    vec![]
                } else {
                    fields[2].split(',').map(str::to_owned).collect()
                };
                assert_eq!(actual, expected, "splitIntoWords {:?}", input());
            }
            "trim" => {
                assert_eq!(
                    hex(util::trim_whitespace(&input())),
                    fields[2],
                    "trimWhitespace {:?}",
                    input()
                );
            }
            _ => continue,
        }
        checked += 1;
    }
    assert!(checked >= 50, "{checked}");
}
