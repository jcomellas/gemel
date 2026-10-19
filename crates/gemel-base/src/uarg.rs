//! Unison-style single-dash command-line flags (`-server`, `-batch`, ...).
//!
//! Ports Unison `src/ubase/uarg.ml`.
//!
//! Derived from Unison, Copyright 1999-2020, Benjamin C. Pierce.
//!
//! Every argument starting with `-` is a keyword, looked up whole (so
//! `--batch` is unknown). A keyword's argument is either attached with `=`
//! (`-batch=false`, `-root=/tmp`) or, except for booleans, the next word.
//! A boolean keyword alone means `true`. Everything else is an anonymous
//! argument. OCaml clients run `<servercmd> -server`, so this syntax is part
//! of the compatibility surface.

use std::fmt;

use crate::util::{int_of_string, split_at_char};

/// What a keyword takes (`Uarg.spec`, without the callback).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Spec {
    /// No argument.
    Unit,
    /// Optional `=true`/`=false`; alone means `true`.
    Bool,
    /// A string.
    String,
    /// An OCaml integer literal.
    Int,
    /// A float.
    Float,
    /// All the remaining arguments.
    Rest,
}

/// The argument passed to a keyword's action.
#[derive(Debug, Clone, PartialEq)]
pub enum Arg {
    /// For [`Spec::Unit`].
    Unit,
    /// For [`Spec::Bool`].
    Bool(bool),
    /// For [`Spec::String`], and each argument after a [`Spec::Rest`] keyword.
    String(String),
    /// For [`Spec::Int`].
    Int(i64),
    /// For [`Spec::Float`].
    Float(f64),
}

/// Why parsing stopped (`Uarg.error`). Its `Display` is the message OCaml
/// prints, without the program name; see [`UsageError::message`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UsageError {
    /// An unknown keyword. `-help` arrives this way too: callers print the
    /// usage to stdout for it instead of an error.
    Unknown(String),
    /// A keyword without its argument.
    Missing(String),
    /// An argument of the wrong form.
    Wrong {
        /// The keyword.
        option: String,
        /// What was given.
        actual: String,
        /// What was expected (`"a boolean"`, `"an integer"`, ...).
        expected: String,
    },
    /// An action or anonymous-argument handler rejected its argument
    /// (`Uarg.Bad`).
    Message(String),
}

impl UsageError {
    /// The line OCaml prints to stderr, e.g. `"unison: unknown option
    /// `-x'."`.
    pub fn message(&self, progname: &str) -> String {
        format!("{progname}: {self}.")
    }
}

impl fmt::Display for UsageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UsageError::Unknown(s) => write!(f, "unknown option `{s}'"),
            UsageError::Missing(s) => write!(f, "option `{s}' needs an argument"),
            UsageError::Wrong {
                option,
                actual,
                expected,
            } => write!(
                f,
                "wrong argument `{actual}'; option `{option}' expects {expected}"
            ),
            UsageError::Message(s) => f.write_str(s),
        }
    }
}

impl std::error::Error for UsageError {}

/// OCaml's `bool_of_string`.
fn bool_of_string(s: &str) -> Option<bool> {
    match s {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

/// OCaml's `float_of_string`, approximately: Rust's float syntax plus `_`
/// separators and hexadecimal integers. Unison has no float options today.
fn float_of_string(s: &str) -> Option<f64> {
    let s: String = s.chars().filter(|&c| c != '_').collect();
    s.parse()
        .ok()
        .or_else(|| int_of_string(&s).map(|n| n as f64))
}

/// `Uarg.parse` over `args` (the arguments after the program name).
///
/// `spec` gives each keyword's [`Spec`], or `None` if it is unknown.
/// `action` and `anon` may reject their argument with a message, which
/// stops parsing with [`UsageError::Message`]. Unlike OCaml, this returns
/// the error rather than printing the usage and exiting with code 2; the
/// caller does that.
pub fn parse(
    args: &[String],
    spec: impl Fn(&str) -> Option<Spec>,
    mut action: impl FnMut(&str, Arg) -> Result<(), String>,
    mut anon: impl FnMut(&str) -> Result<(), String>,
) -> Result<(), UsageError> {
    let mut i = 0;
    while i < args.len() {
        let ss = &args[i];
        if !ss.starts_with('-') {
            anon(ss).map_err(UsageError::Message)?;
            i += 1;
            continue;
        }
        let (s, v) = split_at_char(ss, '=');
        let kind = spec(s).ok_or_else(|| UsageError::Unknown(s.to_owned()))?;
        // The argument: attached with `=`, or the next word.
        let mut take = || match v {
            Some(a) => Ok(a.to_owned()),
            None => {
                i += 1;
                args.get(i)
                    .cloned()
                    .ok_or_else(|| UsageError::Missing(s.to_owned()))
            }
        };
        let wrong = |actual: String, expected: &str| UsageError::Wrong {
            option: s.to_owned(),
            actual,
            expected: expected.to_owned(),
        };
        let arg = match kind {
            Spec::Unit => Arg::Unit,
            Spec::Bool => match v {
                None => Arg::Bool(true),
                Some(_) => {
                    let a = take()?;
                    Arg::Bool(bool_of_string(&a).ok_or_else(|| wrong(a, "a boolean"))?)
                }
            },
            Spec::String => Arg::String(take()?),
            Spec::Int => {
                let a = take()?;
                Arg::Int(int_of_string(&a).ok_or_else(|| wrong(a, "an integer"))?)
            }
            Spec::Float => {
                let a = take()?;
                Arg::Float(float_of_string(&a).ok_or_else(|| wrong(a, "a float"))?)
            }
            Spec::Rest => {
                for rest in &args[i + 1..] {
                    action(s, Arg::String(rest.clone())).map_err(UsageError::Message)?;
                }
                return Ok(());
            }
        };
        action(s, arg).map_err(UsageError::Message)?;
        i += 1;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(args: &[&str]) -> Result<Vec<String>, UsageError> {
        let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        let mut seen = Vec::new();
        let seen_cell = std::cell::RefCell::new(&mut seen);
        parse(
            &args,
            |k| match k {
                "-batch" => Some(Spec::Bool),
                "-root" => Some(Spec::String),
                "-retry" => Some(Spec::Int),
                "-server" => Some(Spec::Unit),
                "-bad" => Some(Spec::String),
                "--" => Some(Spec::Rest),
                _ => None,
            },
            |k, a| {
                if k == "-bad" {
                    return Err("nope".into());
                }
                seen_cell.borrow_mut().push(format!("{k} {a:?}"));
                Ok(())
            },
            |a| {
                seen_cell.borrow_mut().push(format!("anon {a}"));
                Ok(())
            },
        )?;
        Ok(seen)
    }

    #[test]
    fn parses_like_uarg() {
        assert_eq!(
            run(&[
                "-batch",
                "-batch=false",
                "-root",
                "/a",
                "-root=b=c",
                "-retry=0x10",
                "x",
                "-server"
            ])
            .unwrap(),
            [
                "-batch Bool(true)",
                "-batch Bool(false)",
                "-root String(\"/a\")",
                "-root String(\"b=c\")",
                "-retry Int(16)",
                "anon x",
                "-server Unit",
            ]
        );
        assert_eq!(
            run(&["--", "-batch", "y"]).unwrap(),
            ["-- String(\"-batch\")", "-- String(\"y\")"]
        );
    }

    #[test]
    fn reports_errors_like_uarg() {
        assert_eq!(
            run(&["--batch"]),
            Err(UsageError::Unknown("--batch".into()))
        );
        assert_eq!(run(&["-root"]), Err(UsageError::Missing("-root".into())));
        let e = run(&["-batch=yes"]).unwrap_err();
        assert_eq!(
            e.message("unison"),
            "unison: wrong argument `yes'; option `-batch' expects a boolean."
        );
        assert_eq!(
            run(&["-retry", "x"]).unwrap_err().to_string(),
            "wrong argument `x'; option `-retry' expects an integer"
        );
        assert_eq!(run(&["-bad", "v"]), Err(UsageError::Message("nope".into())));
    }
}
