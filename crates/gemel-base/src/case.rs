//! Case-sensitivity and Unicode modes for name comparison and hashing.
//!
//! Ports Unison `src/case.ml`.
//!
//! Derived from Unison, Copyright 1999-2020, Benjamin C. Pierce.
//!
//! The update detector, reconciler and transporter behave differently
//! depending on whether either replica's filesystem is case-insensitive.
//! Both hosts must use the same [`Mode`]: it decides which names are the
//! same file, the order of names in archives, and the hashes in
//! `checkArchive`. The client derives it from preferences at startup
//! ([`init`]) and records it in pseudo-preferences, which travel to the
//! server with the other preferences. Each host reads its mode from its own
//! preferences ([`mode`]).
//!
//! Invariant (from case.ml): if `compare(s, s2)` is equal, then
//! `hash(s) == hash(s2)`, and `normalize_matched_string` gives strings that
//! match the same patterns.

use std::borrow::Cow;
use std::cmp::Ordering;

use gemel_compat::{ocaml_hash, unicode};

use crate::prefs::{BoolWithDefault, Category, Internal, PrefDef, Prefs, Topic};
use crate::util::{Error, lowercase_latin1, nocase_cmp};

/// `ignorecase`: treat names differing only in case as the same file.
/// With `default`, names are case-insensitive if either host is.
pub static IGNORE_CASE: PrefDef<BoolWithDefault> = PrefDef::new(
    "ignorecase",
    Category::Advanced(Topic::Sync),
    BoolWithDefault::default,
    "identify upper/lowercase filenames (true/false/default)",
    "When set to {\\tt true}, this flag causes Unison to treat \
     filenames as case insensitive---i.e., files in the two \
     replicas whose names differ in (upper- and lower-case) `spelling' \
     are treated as the same file.  When the flag is set to {\\tt false}, Unison \
     will treat all filenames as case sensitive.  Ordinarily, when the flag is \
     set to {\\tt default}, \
     filenames are automatically taken to be case-insensitive if \
     either host is running Windows or OSX.  In rare circumstances it may be  \
     useful to set the flag manually.",
);

/// `someHostIsInsensitive`, behind [`Settings::some_host_is_insensitive`].
pub static SOME_HOST_IS_INSENSITIVE: PrefDef<bool> = PrefDef::new(
    "someHostIsInsensitive",
    Category::Internal(Internal::Pseudo),
    || false,
    "*Pseudo-preference for internal use only",
    "",
);

/// `unicode`: assume UTF-8 names in case-insensitive mode, and compare up
/// to decomposition when a host runs macOS (`default` means `true`).
pub static UNICODE: PrefDef<BoolWithDefault> = PrefDef::new(
    "unicode",
    Category::Advanced(Topic::General),
    BoolWithDefault::default,
    "assume Unicode encoding in case insensitive mode",
    "When set to {\\tt true}, this flag causes Unison to perform \
     case insensitive file comparisons assuming Unicode encoding.  \
     This is the default.  When the flag is set to {\\tt false}, \
     Latin 1 encoding is assumed (this means that all bytes that are \
     not letters in Latin 1 encoding will be compared byte-for-byte, \
     even if they may be valid characters in some other encoding).  \
     When Unison runs in case sensitive mode, this flag only makes \
     a difference if one host is running Mac OS X.  \
     Under Mac OS X, it selects whether comparing the filenames up to \
     decomposition, or byte-for-byte.",
);

/// `unicodeEnc`, behind [`Settings::unicode_encoding`].
pub static UNICODE_ENCODING: PrefDef<bool> = PrefDef::new(
    "unicodeEnc",
    Category::Internal(Internal::Pseudo),
    || false,
    "*Pseudo-preference for internal use only",
    "",
);

/// `unicodeCS`, behind [`Settings::unicode_case_sensitive`]. Marked local, so
/// servers that don't know it ignore it.
pub static UNICODE_CASE_SENSITIVE: PrefDef<bool> = PrefDef::new(
    "unicodeCS",
    Category::Internal(Internal::Pseudo),
    || false,
    "*Pseudo-preference for internal use only",
    "",
)
.local();

/// Registers this module's preferences.
pub fn register_prefs(prefs: &mut Prefs) -> Result<(), Error> {
    prefs.register(&IGNORE_CASE)?;
    prefs.register(&SOME_HOST_IS_INSENSITIVE)?;
    prefs.register(&UNICODE)?;
    prefs.register(&UNICODE_ENCODING)?;
    prefs.register(&UNICODE_CASE_SENSITIVE)
}

/// `Case.init`: sets the pseudo-preferences once the client knows whether
/// any root is on a case-insensitive filesystem and whether any host runs
/// macOS.
pub fn init(prefs: &mut Prefs, some_host_is_case_insensitive: bool, some_host_running_osx: bool) {
    Settings::init(
        *prefs.read(&IGNORE_CASE),
        *prefs.read(&UNICODE),
        some_host_is_case_insensitive,
        some_host_running_osx,
    )
    .store(prefs);
}

/// `Case.ops`: the mode in effect on this host.
pub fn mode(prefs: &Prefs) -> Mode {
    Settings::read(prefs).mode()
}

/// `Case.caseSensitiveModeDesc`.
pub const CASE_SENSITIVE_MODE_DESC: &str = "case sensitive";

/// How names are compared, hashed and normalised.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Mode {
    /// Byte-for-byte.
    Sensitive,
    /// Latin-1 case-insensitive.
    Insensitive,
    /// Case-sensitive, but up to decomposition (a macOS host is involved).
    UnicodeSensitive,
    /// Unicode case-insensitive, as macOS and Windows compare names.
    UnicodeInsensitive,
}

impl Mode {
    /// Description of the mode (`modeDesc`). Archives record it, and Unison
    /// compares it on load to notice a change of mode, so it must not change.
    pub fn desc(self) -> &'static str {
        match self {
            Mode::Sensitive => CASE_SENSITIVE_MODE_DESC,
            Mode::Insensitive => "Latin-1 case insensitive",
            Mode::UnicodeSensitive => "Unicode case sensitive",
            Mode::UnicodeInsensitive => "Unicode case insensitive",
        }
    }

    /// Compares two names.
    pub fn compare(self, s: &[u8], s2: &[u8]) -> Ordering {
        match self {
            Mode::Sensitive => s.cmp(s2),
            Mode::Insensitive => nocase_cmp(s, s2),
            Mode::UnicodeSensitive => unicode::case_sensitive_compare(s, s2),
            Mode::UnicodeInsensitive => unicode::case_insensitive_compare(s, s2),
        }
    }

    /// Hashes a name consistently with [`Mode::compare`] (`Uutil.hash` of the
    /// normalised name).
    pub fn hash(self, s: &[u8]) -> i64 {
        match self {
            Mode::Sensitive => ocaml_hash::hash_string(s),
            Mode::Insensitive => {
                let lower: Vec<u8> = s.iter().map(|&c| lowercase_latin1(c)).collect();
                ocaml_hash::hash_string(&lower)
            }
            Mode::UnicodeSensitive => ocaml_hash::hash_string(&unicode::decompose(s)),
            Mode::UnicodeInsensitive => ocaml_hash::hash_string(&unicode::normalize(s)),
        }
    }

    /// Normalises an ignore pattern before compiling it.
    pub fn normalize_pattern(self, p: &[u8]) -> Cow<'_, [u8]> {
        self.normalize_matched_string(p)
    }

    /// Whether patterns must be compiled case-insensitively.
    pub fn case_insensitive_match(self) -> bool {
        self == Mode::Insensitive
    }

    /// Normalises a name before matching it against compiled patterns.
    pub fn normalize_matched_string(self, s: &[u8]) -> Cow<'_, [u8]> {
        match self {
            Mode::Sensitive | Mode::Insensitive => Cow::Borrowed(s),
            Mode::UnicodeSensitive => Cow::Owned(unicode::decompose(s)),
            Mode::UnicodeInsensitive => Cow::Owned(unicode::normalize(s)),
        }
    }

    /// Normalises a name read from the filesystem (recomposing the names
    /// macOS decomposes).
    pub fn normalize_filename(self, s: &[u8]) -> Cow<'_, [u8]> {
        match self {
            Mode::Sensitive | Mode::Insensitive => Cow::Borrowed(s),
            Mode::UnicodeSensitive | Mode::UnicodeInsensitive => Cow::Owned(unicode::compose(s)),
        }
    }

    /// Whether a name can't be represented in this mode.
    pub fn bad_encoding(self, s: &[u8]) -> bool {
        match self {
            Mode::Sensitive | Mode::Insensitive => false,
            Mode::UnicodeSensitive | Mode::UnicodeInsensitive => !unicode::check_utf_8(s),
        }
    }
}

/// The settings [`Mode`] is chosen from: the three pseudo-preferences that
/// `Case.init` sets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Settings {
    /// `someHostIsInsensitive`, sent to the server.
    pub some_host_is_insensitive: bool,
    /// `unicodeEnc`, sent to the server.
    pub unicode_encoding: bool,
    /// `unicodeCS`, sent to the server (but ignored by those that don't
    /// know it).
    pub unicode_case_sensitive: bool,
}

impl Settings {
    /// `Case.init`: derives the settings from the `ignorecase` and `unicode`
    /// preferences once the client knows whether any root is on a
    /// case-insensitive filesystem and whether any host runs macOS.
    pub fn init(
        ignore_case: BoolWithDefault,
        unicode: BoolWithDefault,
        some_host_is_case_insensitive: bool,
        some_host_running_osx: bool,
    ) -> Self {
        let use_unicode = unicode != BoolWithDefault::False;
        Settings {
            some_host_is_insensitive: ignore_case == BoolWithDefault::True
                || (ignore_case == BoolWithDefault::Default && some_host_is_case_insensitive),
            unicode_case_sensitive: use_unicode && some_host_running_osx,
            unicode_encoding: use_unicode,
        }
    }

    /// The settings in `prefs`.
    pub fn read(prefs: &Prefs) -> Self {
        Settings {
            some_host_is_insensitive: *prefs.read(&SOME_HOST_IS_INSENSITIVE),
            unicode_encoding: *prefs.read(&UNICODE_ENCODING),
            unicode_case_sensitive: *prefs.read(&UNICODE_CASE_SENSITIVE),
        }
    }

    /// Sets the pseudo-preferences in `prefs`.
    pub fn store(self, prefs: &mut Prefs) {
        prefs.set(&SOME_HOST_IS_INSENSITIVE, self.some_host_is_insensitive);
        prefs.set(&UNICODE_ENCODING, self.unicode_encoding);
        prefs.set(&UNICODE_CASE_SENSITIVE, self.unicode_case_sensitive);
    }

    /// `Case.ops`: the mode these settings select.
    pub fn mode(self) -> Mode {
        if self.some_host_is_insensitive {
            if self.unicode_encoding {
                Mode::UnicodeInsensitive
            } else {
                Mode::Insensitive
            }
        } else if self.unicode_case_sensitive {
            Mode::UnicodeSensitive
        } else {
            Mode::Sensitive
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use BoolWithDefault::{Default as Auto, False, True};

    #[test]
    fn init_selects_modes_like_case_ml() {
        let mode = |ic, uni, insensitive, osx| Settings::init(ic, uni, insensitive, osx).mode();
        assert_eq!(mode(Auto, Auto, false, false), Mode::Sensitive);
        assert_eq!(mode(Auto, Auto, false, true), Mode::UnicodeSensitive);
        assert_eq!(mode(Auto, Auto, true, true), Mode::UnicodeInsensitive);
        assert_eq!(mode(Auto, False, true, true), Mode::Insensitive);
        assert_eq!(mode(Auto, False, false, true), Mode::Sensitive);
        assert_eq!(mode(True, True, false, false), Mode::UnicodeInsensitive);
        assert_eq!(mode(False, Auto, true, false), Mode::Sensitive);
    }

    #[test]
    fn init_stores_the_mode_in_pseudo_preferences() {
        let mut prefs = Prefs::new();
        register_prefs(&mut prefs).unwrap();
        assert_eq!(mode(&prefs), Mode::Sensitive);
        init(&mut prefs, true, true);
        assert_eq!(mode(&prefs), Mode::UnicodeInsensitive);
        prefs.set(&UNICODE, False);
        init(&mut prefs, true, false);
        assert_eq!(mode(&prefs), Mode::Insensitive);
        prefs.set(&IGNORE_CASE, False);
        prefs.set(&UNICODE, Auto);
        init(&mut prefs, true, true);
        assert_eq!(mode(&prefs), Mode::UnicodeSensitive);
        // All three travel to the server; unicodeCS as optional.
        let dump = prefs.dump(1).unwrap();
        for name in ["someHostIsInsensitive", "unicodeEnc", "unicodeCS"] {
            let (_, optional, _) = dump.iter().find(|(n, _, _)| n == name).unwrap();
            assert_eq!(*optional, name == "unicodeCS");
        }
    }

    #[test]
    fn insensitive_mode_hashes_equal_names_equally() {
        let m = Mode::Insensitive;
        assert_eq!(m.compare(b"Caf\xc9", b"caf\xe9"), Ordering::Equal);
        assert_eq!(m.hash(b"Caf\xc9"), m.hash(b"caf\xe9"));
    }
}
