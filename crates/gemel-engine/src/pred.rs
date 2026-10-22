//! Path predicates (`ignore`, `follow`, ...).
//!
//! Ports Unison `src/pred.ml`, and `followPred` from Unison `src/path.ml`.
//!
//! Derived from Unison, Copyright 1999-2020, Benjamin C. Pierce.
//!
//! A predicate is a string-list preference of patterns, each of the form
//! `<TYPE> <PAT> [-> <ASSOCIATED STRING>]`, with `Name`, `Path`,
//! `BelowPath` or `Regex` as the type. OCaml compiles the list into one
//! regular expression and recompiles it whenever the preference or the
//! case mode changes. Here a [`Pred`] is a `static` holding its preference;
//! [`Pred::compiled`] gives a [`Compiled`] predicate for the current
//! patterns and case mode, an immutable value that can be handed to the
//! blocking threads that scan replicas. It is cached by patterns and mode,
//! which is all it depends on.
//!
//! OCaml's default patterns (`addDefaultPatterns`) and initial values
//! (`?initial`) are never used by Unison itself, so they are not ported.

use std::sync::{Arc, Mutex};

use gemel_base::case::Mode;
use gemel_base::features::Features;
use gemel_base::prefs::{Category, PrefDef, Prefs, Topic};
use gemel_base::rx::Rx;
use gemel_base::util::{self, Error};

/// `Pred.mapSeparator`: what separates a pattern from its associated
/// string.
pub const MAP_SEPARATOR: &str = "->";

/// A predicate over paths, with its preference (`Pred.t`).
pub struct Pred {
    pref: PrefDef<Vec<String>>,
    cache: Mutex<Option<Arc<Compiled>>>,
}

impl Pred {
    /// `Pred.create`: a predicate whose preference is called `name`. `doc`
    /// should be "add a pattern to the NAME list", as OCaml generates it.
    pub const fn new(
        name: &'static str,
        category: Category,
        doc: &'static str,
        fulldoc: &'static str,
    ) -> Pred {
        Pred::with_initial(name, category, Vec::new, doc, fulldoc)
    }

    /// `Pred.create ~initial`: a predicate whose patterns start as
    /// `initial`'s.
    pub const fn with_initial(
        name: &'static str,
        category: Category,
        initial: fn() -> Vec<String>,
        doc: &'static str,
        fulldoc: &'static str,
    ) -> Pred {
        Pred {
            pref: PrefDef::new(name, category, initial, doc, fulldoc).intern(add_pattern),
            cache: Mutex::new(None),
        }
    }

    /// `~local:true`: see [`PrefDef::local`].
    pub const fn local(mut self) -> Pred {
        self.pref = self.pref.local();
        self
    }

    /// `~send`: see [`PrefDef::send`].
    pub const fn send(mut self, send: fn(&Prefs, &Features) -> bool) -> Pred {
        self.pref = self.pref.send(send);
        self
    }

    /// The predicate's preference.
    pub fn pref(&'static self) -> &'static PrefDef<Vec<String>> {
        &self.pref
    }

    /// Registers the predicate's preference.
    pub fn register(&'static self, prefs: &mut Prefs) -> Result<(), Error> {
        prefs.register(&self.pref)
    }

    /// `Pred.alias`.
    pub fn alias(&'static self, prefs: &mut Prefs, name: &'static str) -> Result<(), Error> {
        prefs.alias(&self.pref, name)
    }

    /// `Pred.intern`: replaces the patterns.
    pub fn intern(&self, prefs: &mut Prefs, patterns: Vec<String>) {
        prefs.set(&self.pref, patterns);
    }

    /// `Pred.extern`: the patterns, newest first.
    pub fn patterns(&self, prefs: &Prefs) -> Vec<String> {
        prefs.read(&self.pref).clone()
    }

    /// The predicate for the current patterns and case mode. Fails only if
    /// a pattern is malformed, which can happen only if it was set without
    /// parsing (from a server's preference dump, say), as in OCaml.
    pub fn compiled(&self, prefs: &Prefs, mode: Mode) -> Result<Arc<Compiled>, Error> {
        let patterns = prefs.read(&self.pref);
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(c) = &*cache
            && c.mode == mode
            && c.patterns == *patterns
        {
            return Ok(c.clone());
        }
        let c = Arc::new(Compiled::new(patterns.clone(), mode).map_err(Error::Fatal)?);
        *cache = Some(c.clone());
        Ok(c)
    }

    /// `Pred.test`.
    pub fn test(&self, prefs: &Prefs, mode: Mode, s: &[u8]) -> Result<bool, Error> {
        Ok(self.compiled(prefs, mode)?.test(s))
    }
}

/// The preference's parser: checks the pattern and adds it first.
#[allow(clippy::ptr_arg)] // The signature of `PrefDef::intern`.
fn add_pattern(old: &Vec<String>, s: &str) -> Result<Vec<String>, String> {
    // OCaml checks with the case mode in effect, which doesn't change
    // whether a pattern is well formed.
    compile_pattern(s, Mode::Sensitive)?;
    Ok(std::iter::once(s.to_owned())
        .chain(old.iter().cloned())
        .collect())
}

/// A predicate compiled for a list of patterns and a case mode.
#[derive(Debug)]
pub struct Compiled {
    mode: Mode,
    patterns: Vec<String>,
    rx: Rx,
    associated: Vec<(Rx, String)>,
}

impl Compiled {
    /// `Pred.recompile`: compiles `patterns` for `mode`.
    pub fn new(patterns: Vec<String>, mode: Mode) -> Result<Compiled, String> {
        let compiled = patterns
            .iter()
            .map(|p| compile_pattern(p, mode))
            .collect::<Result<Vec<_>, _>>()?;
        let handle_case = |rx: Rx| {
            if mode.case_insensitive_match() {
                rx.case_insensitive()
            } else {
                rx
            }
        };
        let rx = handle_case(Rx::alt(
            &compiled
                .iter()
                .map(|(rx, _)| rx.clone())
                .collect::<Vec<_>>(),
        ));
        let associated = compiled
            .into_iter()
            .filter_map(|(rx, v)| v.map(|v| (handle_case(rx), v)))
            .collect();
        Ok(Compiled {
            mode,
            patterns,
            rx,
            associated,
        })
    }

    /// `Pred.test`: whether `s` matches one of the patterns.
    pub fn test(&self, s: &[u8]) -> bool {
        self.rx.match_string(self.mode.normalize_matched_string(s))
    }

    /// `Pred.assoc`: the associated string of the first pattern with one
    /// that `s` matches.
    pub fn assoc(&self, s: &[u8]) -> Option<&str> {
        let s = self.mode.normalize_matched_string(s);
        self.associated
            .iter()
            .find(|(rx, _)| rx.match_string(&s))
            .map(|(_, v)| v.as_str())
    }

    /// `Pred.assoc_all`: the associated strings of all the patterns that
    /// `s` matches.
    pub fn assoc_all(&self, s: &[u8]) -> Vec<&str> {
        let s = self.mode.normalize_matched_string(s);
        self.associated
            .iter()
            .filter(|(rx, _)| rx.match_string(&s))
            .map(|(_, v)| v.as_str())
            .collect()
    }

    /// `Pred.extern_associated_strings`.
    pub fn associated_strings(&self) -> Vec<&str> {
        self.associated.iter().map(|(_, v)| v.as_str()).collect()
    }
}

fn error_msg(p: &str) -> String {
    format!(
        "bad pattern: {p}\n\
         A pattern must be introduced by one of the following keywords:\n\
         \x20   Name, Path, BelowPath or Regex."
    )
}

/// `Pred.compile_pattern`: a pattern's regular expression and associated
/// string. Errors are OCaml's `IllegalValue` messages.
pub fn compile_pattern(clause: &str, mode: Mode) -> Result<(Rx, Option<String>), String> {
    // Surround by spaces to find "(^| )->( |$)", so that an empty pattern
    // or an empty string is detected.
    let sep = format!(" {MAP_SEPARATOR} ");
    let padded = format!(" {clause} ");
    let (p, v) = match util::split_at_string(&padded, &sep, true) {
        ("", _) => return Err("Empty pattern".into()),
        (p, v) => (p, v.map(|v| util::trim_whitespace(v).to_owned())),
    };
    // Without the space prepended above.
    let body = &p[1..];
    let malformed = || format!("Malformed pattern \"{p}\".");
    let no_slash = |kind: &str| {
        format!(
            "Malformed pattern: \"{p}\"\n\
             '{kind}' patterns may not begin with a slash; only relative paths are allowed."
        )
    };
    let arg = |keyword: &str| {
        body.strip_prefix(keyword).map(|rest| {
            mode.normalize_pattern(util::trim_whitespace(rest).as_bytes())
                .into_owned()
        })
    };
    let rx = |r: Result<Rx, _>| r.map_err(|_| malformed());
    let compiled = if let Some(s) = arg("Name ") {
        Rx::seq(&[rx(Rx::regex("(.*/)?"))?, rx(Rx::globx(&s))?])
    } else if let Some(s) = arg("Path ") {
        if s.first() == Some(&b'/') {
            return Err(no_slash("Path"));
        }
        rx(Rx::globx(&s))?
    } else if let Some(s) = arg("BelowPath ") {
        if s.first() == Some(&b'/') {
            return Err(no_slash("BelowPath"));
        }
        Rx::seq(&[rx(Rx::globx(&s))?, rx(Rx::regex("(/.*)?"))?])
    } else if let Some(s) = arg("Regex ") {
        rx(Rx::regex(&s))?
    } else {
        return Err(error_msg(p));
    };
    Ok((compiled, v))
}

/// `follow` (`Path.followPred`): symbolic links to treat as what they point
/// to.
pub static FOLLOW: Pred = Pred::new(
    "follow",
    Category::Advanced(Topic::Sync),
    "add a pattern to the follow list",
    "Including the preference \\texttt{-follow \\ARG{pathspec}} causes Unison to \
     treat symbolic links matching \\ARG{pathspec} as `invisible' and \
     behave as if the object pointed to by the link had appeared literally \
     at this position in the replica.  See \
     \\sectionref{symlinks}{Symbolic Links} for more details. \
     The syntax of \\ARG{pathspec} is \
     described in \\sectionref{pathspec}{Path Specification}.",
);

/// Registers this module's preferences.
pub fn register_prefs(prefs: &mut Prefs) -> Result<(), Error> {
    FOLLOW.register(prefs)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn compiled(patterns: &[&str], mode: Mode) -> Compiled {
        Compiled::new(patterns.iter().map(|&s| s.to_owned()).collect(), mode).unwrap()
    }

    #[test]
    fn pattern_kinds() {
        let c = compiled(
            &["Name *.o", "Path a/b", "BelowPath c", "Regex x[0-9]+"],
            Mode::Sensitive,
        );
        for (s, m) in [
            ("foo.o", true),
            ("d/foo.o", true),
            ("a/b", true),
            ("a/b/c", false),
            ("c", true),
            ("c/d/e", true),
            ("cc", false),
            ("x12", true),
            ("y/x1", false),
        ] {
            assert_eq!(c.test(s.as_bytes()), m, "{s}");
        }
    }

    #[test]
    fn associated_strings_split_at_the_last_arrow() {
        let c = compiled(
            &[
                "Name *.c -> cc",
                "Name * -> any",
                "Name *.h",
                "Name a -> b -> c",
            ],
            Mode::Sensitive,
        );
        assert_eq!(c.assoc(b"f.c"), Some("cc"));
        assert_eq!(c.assoc_all(b"f.c"), ["cc", "any"]);
        assert_eq!(c.assoc(b"f.h"), Some("any"));
        assert!(c.test(b"f.h"));
        // The pattern is everything before the last arrow.
        assert_eq!(c.assoc_all(b"a -> b"), ["any", "c"]);
        assert_eq!(c.associated_strings(), ["cc", "any", "c"]);
    }

    #[test]
    fn errors() {
        let err = |s| compile_pattern(s, Mode::Sensitive).unwrap_err();
        assert_eq!(err("-> x"), "Empty pattern");
        assert_eq!(
            err("Foo bar"),
            "bad pattern:  Foo bar \nA pattern must be introduced by one of the following \
             keywords:\n    Name, Path, BelowPath or Regex."
        );
        assert!(err("Path /a").starts_with("Malformed pattern: \" Path /a \"\n'Path' patterns"));
        assert_eq!(err("Regex a("), "Malformed pattern \" Regex a( \".");
    }

    #[test]
    fn case_insensitive_mode() {
        let c = compiled(&["Name *.TXT"], Mode::Insensitive);
        assert!(c.test(b"a.txt") && c.test(b"A.Txt"));
        assert!(!compiled(&["Name *.TXT"], Mode::Sensitive).test(b"a.txt"));
    }

    #[test]
    fn the_preference_checks_and_prepends() {
        let mut prefs = Prefs::new();
        FOLLOW.register(&mut prefs).unwrap();
        let args = |a: &[&str]| a.iter().map(|&s| s.to_owned()).collect::<Vec<_>>();
        prefs
            .parse_cmd_line(&args(&["-follow", "Name a", "-follow", "Name b"]))
            .unwrap();
        assert_eq!(FOLLOW.patterns(&prefs), ["Name b", "Name a"]);
        assert!(
            prefs
                .parse_cmd_line(&args(&["-follow", "nonsense"]))
                .is_err()
        );
        assert!(FOLLOW.test(&prefs, Mode::Sensitive, b"x/a").unwrap());
        let first = FOLLOW.compiled(&prefs, Mode::Sensitive).unwrap();
        assert!(Arc::ptr_eq(
            &first,
            &FOLLOW.compiled(&prefs, Mode::Sensitive).unwrap()
        ));
        FOLLOW.intern(&mut prefs, vec!["Name c".into()]);
        assert!(!FOLLOW.test(&prefs, Mode::Sensitive, b"x/a").unwrap());
    }
}
