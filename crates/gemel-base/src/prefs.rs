//! Preferences and `.prf` profile parsing (file-format compatible).
//!
//! Ports Unison `src/ubase/prefs.ml`.
//!
//! Derived from Unison, Copyright 1999-2002.
//!
//! # Declaring and reading preferences
//!
//! In OCaml every module creates its preferences at load time in a global
//! table. Here a module declares each one as a static [`PrefDef`], and the
//! program registers the definitions it links into a [`Prefs`] value owned by
//! the session, which holds the current values:
//!
//! ```
//! use gemel_base::prefs::{Category, PrefDef, Prefs, Topic};
//!
//! static BATCH: PrefDef<bool> = PrefDef::new(
//!     "batch",
//!     Category::Basic(Topic::Syncprocess_CLI),
//!     || false,
//!     "batch mode: ask no questions at all",
//!     "",
//! );
//!
//! let mut prefs = Prefs::new();
//! prefs.register(&BATCH).unwrap();
//! prefs.parse_cmd_line(&["-batch".to_owned()]).unwrap();
//! assert!(*prefs.read(&BATCH));
//! ```
//!
//! # Compatibility
//!
//! Three things here must match OCaml Unison: the profile syntax
//! ([`Prefs::load_the_file`]), the command-line syntax ([`crate::uarg`]), and
//! the preference dump the client sends the server ([`Prefs::dump`]): a list
//! of `(name, local, value)` where `value` is the Umarshal encoding, header
//! included, of the preference's value and its names.
//!
//! The usage text and the TeX/man documentation printers are not ported yet.

use std::any::Any;
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use gemel_compat::umarshal::{self, Reader, Writer};

use crate::uarg::{self, Arg, Spec, UsageError};
use crate::util::{
    Error, int_of_string, remove_trailing_cr, split_at_char, split_into_words, trim_whitespace,
};

/// Value of a three-state boolean preference (`createBoolWithDefault`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BoolWithDefault {
    /// `true` or `yes`.
    True,
    /// Anything else.
    False,
    /// `default` or `auto`: let Unison decide.
    #[default]
    Default,
}

/// Documentation topic of a preference.
#[allow(non_camel_case_types)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Topic {
    /// General.
    General,
    /// What to sync.
    Sync,
    /// How to sync.
    Syncprocess,
    /// How to sync (text interface only).
    Syncprocess_CLI,
    /// Text interface.
    CLI,
    /// Graphical interface.
    GUI,
    /// Remote connections.
    Remote,
    /// Archive management.
    Archive,
}

impl Topic {
    /// `Prefs.topic`: the section heading.
    pub fn title(self) -> &'static str {
        match self {
            Topic::General => "General",
            Topic::Sync => "What to sync",
            Topic::Syncprocess => "How to sync",
            Topic::Syncprocess_CLI => "How to sync (text interface (CLI) only)",
            Topic::CLI => "Text interface (CLI)",
            Topic::GUI => "Graphical interface (GUI)",
            Topic::Remote => "Remote connections",
            Topic::Archive => "Archive management",
        }
    }
}

/// Kind of internal preference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Internal {
    /// Set by Unison itself, never by users (e.g. `someHostIsInsensitive`).
    Pseudo,
    /// For developers.
    Devel,
    /// Other undocumented preferences, including aliases.
    Other,
}

/// Where a preference is documented (`Prefs.group`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Category {
    /// A basic option.
    Basic(Topic),
    /// An advanced option.
    Advanced(Topic),
    /// An expert option.
    Expert,
    /// Not documented.
    Internal(Internal),
}

impl Category {
    fn is_internal(self) -> bool {
        matches!(self, Category::Internal(_))
    }
}

/// The type of a preference, as far as parsing is concerned (`Prefs.typ`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    /// `createBool`.
    Bool,
    /// `createInt`.
    Int,
    /// `createString`.
    String,
    /// `createStringList`: each setting adds to the list.
    StringList,
    /// `createBoolWithDefault`.
    BoolDef,
    /// `create`, with its own parser.
    Custom,
}

impl Kind {
    fn spec(self) -> Spec {
        match self {
            Kind::Bool => Spec::Bool,
            Kind::Int => Spec::Int,
            _ => Spec::String,
        }
    }
}

/// A setting from the command line or a profile, already checked against
/// the preference's [`Kind`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Input<'a> {
    /// For [`Kind::Bool`].
    Bool(bool),
    /// For [`Kind::Int`].
    Int(i64),
    /// For every other kind.
    Str(&'a str),
}

/// A type that can be a preference's value.
pub trait PrefValue: Clone + PartialEq + Send + Sync + 'static {
    /// How settings are parsed.
    const KIND: Kind;
    /// The new value after a setting. String lists add to the current value;
    /// the other built-in kinds replace it. An error becomes OCaml's
    /// `IllegalValue`.
    fn apply(&self, input: Input<'_>) -> Result<Self, String>;
    /// The value as profile lines' right-hand sides (`printer`).
    fn print(&self) -> Vec<String>;
    /// Umarshal encoding, for the dump sent to the server.
    fn marshal(&self, w: &mut Writer);
    /// Umarshal decoding.
    fn unmarshal(r: &mut Reader<'_>) -> Result<Self, DecodeError>;
}

/// Why a preference value received from the client couldn't be decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    /// Malformed Umarshal data.
    Umarshal(umarshal::Error),
    /// A string that isn't UTF-8. OCaml strings are bytes, but gemel keeps
    /// preference values as text; such a value is rejected rather than
    /// altered, since it may be a path.
    NotUtf8,
}

impl From<umarshal::Error> for DecodeError {
    fn from(e: umarshal::Error) -> Self {
        DecodeError::Umarshal(e)
    }
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecodeError::Umarshal(e) => e.fmt(f),
            DecodeError::NotUtf8 => f.write_str("a string value is not valid UTF-8"),
        }
    }
}

impl std::error::Error for DecodeError {}

impl PrefValue for bool {
    const KIND: Kind = Kind::Bool;
    fn apply(&self, input: Input<'_>) -> Result<Self, String> {
        match input {
            Input::Bool(b) => Ok(b),
            other => Err(format!("expected a boolean, got {other:?}")),
        }
    }
    fn print(&self) -> Vec<String> {
        vec![self.to_string()]
    }
    fn marshal(&self, w: &mut Writer) {
        w.bool(*self);
    }
    fn unmarshal(r: &mut Reader<'_>) -> Result<Self, DecodeError> {
        Ok(r.bool()?)
    }
}

impl PrefValue for i64 {
    const KIND: Kind = Kind::Int;
    fn apply(&self, input: Input<'_>) -> Result<Self, String> {
        match input {
            Input::Int(n) => Ok(n),
            other => Err(format!("expected an integer, got {other:?}")),
        }
    }
    fn print(&self) -> Vec<String> {
        vec![self.to_string()]
    }
    fn marshal(&self, w: &mut Writer) {
        w.int(*self);
    }
    fn unmarshal(r: &mut Reader<'_>) -> Result<Self, DecodeError> {
        Ok(r.int()?)
    }
}

fn str_input(input: Input<'_>) -> Result<&str, String> {
    match input {
        Input::Str(s) => Ok(s),
        other => Err(format!("expected a string, got {other:?}")),
    }
}

/// A string value, which must be UTF-8 (see [`DecodeError::NotUtf8`]).
fn read_string(r: &mut Reader<'_>) -> Result<String, DecodeError> {
    String::from_utf8(r.string()?).map_err(|_| DecodeError::NotUtf8)
}

impl PrefValue for String {
    const KIND: Kind = Kind::String;
    fn apply(&self, input: Input<'_>) -> Result<Self, String> {
        str_input(input).map(str::to_owned)
    }
    fn print(&self) -> Vec<String> {
        vec![self.clone()]
    }
    fn marshal(&self, w: &mut Writer) {
        w.string(self.as_bytes());
    }
    fn unmarshal(r: &mut Reader<'_>) -> Result<Self, DecodeError> {
        read_string(r)
    }
}

/// A string list: each setting is prepended, so the newest comes first.
impl PrefValue for Vec<String> {
    const KIND: Kind = Kind::StringList;
    fn apply(&self, input: Input<'_>) -> Result<Self, String> {
        let s = str_input(input)?;
        Ok(std::iter::once(s.to_owned())
            .chain(self.iter().cloned())
            .collect())
    }
    fn print(&self) -> Vec<String> {
        self.clone()
    }
    fn marshal(&self, w: &mut Writer) {
        w.list(self, |w, s| w.string(s.as_bytes()));
    }
    fn unmarshal(r: &mut Reader<'_>) -> Result<Self, DecodeError> {
        r.list(Reader::string)?
            .into_iter()
            .map(|s| String::from_utf8(s).map_err(|_| DecodeError::NotUtf8))
            .collect()
    }
}

impl PrefValue for BoolWithDefault {
    const KIND: Kind = Kind::BoolDef;
    fn apply(&self, input: Input<'_>) -> Result<Self, String> {
        Ok(match str_input(input)? {
            "yes" | "true" => BoolWithDefault::True,
            "default" | "auto" => BoolWithDefault::Default,
            _ => BoolWithDefault::False,
        })
    }
    fn print(&self) -> Vec<String> {
        let s = match self {
            BoolWithDefault::True => "true",
            BoolWithDefault::False => "false",
            BoolWithDefault::Default => "default",
        };
        vec![s.to_owned()]
    }
    fn marshal(&self, w: &mut Writer) {
        w.tag(match self {
            BoolWithDefault::True => 0,
            BoolWithDefault::False => 1,
            BoolWithDefault::Default => 2,
        });
    }
    fn unmarshal(r: &mut Reader<'_>) -> Result<Self, DecodeError> {
        Ok(match r.tag(3)? {
            0 => BoolWithDefault::True,
            1 => BoolWithDefault::False,
            _ => BoolWithDefault::Default,
        })
    }
}

/// The value of `source` and `include`, which act when set.
impl PrefValue for () {
    const KIND: Kind = Kind::Custom;
    fn apply(&self, _: Input<'_>) -> Result<Self, String> {
        Ok(())
    }
    fn print(&self) -> Vec<String> {
        Vec::new()
    }
    fn marshal(&self, _: &mut Writer) {}
    fn unmarshal(_: &mut Reader<'_>) -> Result<Self, DecodeError> {
        Ok(())
    }
}

/// A custom parser: the new value from the current one and a setting.
pub type Intern<T> = fn(&T, &str) -> Result<T, String>;

/// A preference's definition, declared as a `static` by the module that
/// owns it and registered with [`Prefs::register`].
pub struct PrefDef<T> {
    name: &'static str,
    category: Category,
    default: fn() -> T,
    doc: &'static str,
    fulldoc: &'static str,
    cli_only: bool,
    local: bool,
    deprecated: bool,
    send: Option<fn(&Prefs) -> bool>,
    intern: Option<Intern<T>>,
    printer: Option<fn(&T) -> Vec<String>>,
}

impl<T> PrefDef<T> {
    /// A preference named `name` with a default value, a one-line `doc` and
    /// a full TeX `fulldoc`.
    pub const fn new(
        name: &'static str,
        category: Category,
        default: fn() -> T,
        doc: &'static str,
        fulldoc: &'static str,
    ) -> Self {
        PrefDef {
            name,
            category,
            default,
            doc,
            fulldoc,
            cli_only: false,
            local: false,
            deprecated: false,
            send: None,
            intern: None,
            printer: None,
        }
    }

    /// Command line only: rejected in profiles and never sent to the server.
    pub const fn cli_only(mut self) -> Self {
        self.cli_only = true;
        self
    }

    /// Sent to the server as optional: a server that doesn't know it
    /// ignores it rather than failing (`~local:true`).
    pub const fn local(mut self) -> Self {
        self.local = true;
        self
    }

    /// Deprecated: setting it warns.
    pub const fn deprecated(mut self) -> Self {
        self.deprecated = true;
        self
    }

    /// Sent to the server only when `send` returns true (`?send`).
    pub const fn send(mut self, send: fn(&Prefs) -> bool) -> Self {
        self.send = Some(send);
        self
    }

    /// A custom parser (`Prefs.create`'s `intern`), given the current value
    /// and the setting. Makes the preference [`Kind::Custom`].
    pub const fn intern(mut self, intern: Intern<T>) -> Self {
        self.intern = Some(intern);
        self
    }

    /// A custom printer (`Prefs.create`'s `printer`).
    pub const fn printer(mut self, printer: fn(&T) -> Vec<String>) -> Self {
        self.printer = Some(printer);
        self
    }

    /// The preference's name.
    pub fn name(&self) -> &'static str {
        self.name
    }

    /// `Prefs.readDefault`.
    pub fn default_value(&self) -> T {
        (self.default)()
    }
}

type BoxedValue = Box<dyn Any + Send + Sync>;

/// [`PrefDef`] with its type erased, so that `Prefs` can hold them all.
trait AnyPref: Sync {
    fn name(&self) -> &'static str;
    fn category(&self) -> Category;
    fn kind(&self) -> Kind;
    fn doc(&self) -> String;
    fn fulldoc(&self) -> &'static str;
    fn cli_only(&self) -> bool;
    fn local(&self) -> bool;
    fn deprecated(&self) -> bool;
    fn should_send(&self, prefs: &Prefs) -> bool;
    fn default_value(&self) -> BoxedValue;
    fn is_default(&self, v: &BoxedValue) -> bool;
    fn apply(&self, v: &BoxedValue, input: Input<'_>) -> Result<BoxedValue, String>;
    fn print(&self, v: &BoxedValue) -> Vec<String>;
    fn dump(&self, v: &BoxedValue, names: &[String]) -> Vec<u8>;
    fn load(&self, bytes: &[u8]) -> Result<BoxedValue, DecodeError>;
}

fn downcast<T: 'static>(v: &BoxedValue) -> &T {
    v.downcast_ref()
        .expect("preference value of the registered type")
}

impl<T: PrefValue> AnyPref for PrefDef<T> {
    fn name(&self) -> &'static str {
        self.name
    }
    fn category(&self) -> Category {
        self.category
    }
    fn kind(&self) -> Kind {
        if self.intern.is_some() {
            Kind::Custom
        } else {
            T::KIND
        }
    }
    fn doc(&self) -> String {
        // createBool documents a true default.
        let default_true = T::KIND == Kind::Bool && (self.default)().print() == ["true"];
        if default_true {
            format!("{} (default true)", self.doc)
        } else {
            self.doc.to_owned()
        }
    }
    fn fulldoc(&self) -> &'static str {
        self.fulldoc
    }
    fn cli_only(&self) -> bool {
        self.cli_only
    }
    fn local(&self) -> bool {
        // createPrefInternal: command-line-only preferences are local and
        // never sent.
        self.local || self.cli_only
    }
    fn deprecated(&self) -> bool {
        self.deprecated
    }
    fn should_send(&self, prefs: &Prefs) -> bool {
        !self.cli_only && self.send.is_none_or(|f| f(prefs))
    }
    fn default_value(&self) -> BoxedValue {
        Box::new((self.default)())
    }
    fn is_default(&self, v: &BoxedValue) -> bool {
        *downcast::<T>(v) == (self.default)()
    }
    fn apply(&self, v: &BoxedValue, input: Input<'_>) -> Result<BoxedValue, String> {
        let v = downcast::<T>(v);
        let new = match (self.intern, input) {
            (Some(intern), Input::Str(s)) => intern(v, s)?,
            _ => v.apply(input)?,
        };
        Ok(Box::new(new))
    }
    fn print(&self, v: &BoxedValue) -> Vec<String> {
        let v = downcast::<T>(v);
        self.printer.map_or_else(|| v.print(), |p| p(v))
    }
    fn dump(&self, v: &BoxedValue, names: &[String]) -> Vec<u8> {
        let v = downcast::<T>(v);
        umarshal::to_bytes(|w| {
            v.marshal(w);
            w.list(names, |w, n| w.string(n.as_bytes()));
        })
    }
    fn load(&self, bytes: &[u8]) -> Result<BoxedValue, DecodeError> {
        let v = umarshal::from_bytes(bytes, |r| {
            let v = T::unmarshal(r);
            // The names are sent too, but only the value is used.
            r.list(Reader::string)?;
            Ok(v)
        })??;
        Ok(Box::new(v))
    }
}

struct Cell {
    def: &'static dyn AnyPref,
    value: BoxedValue,
    set_in_profile: bool,
    names: Vec<String>,
}

/// One preference in a dump: name, whether a server may ignore it, and the
/// Umarshal encoding of its value and names (`Prefs.dumpedPrefs`).
pub type DumpedPref = (String, bool, Vec<u8>);

/// Where a profile line came from: a description of the file, and the line
/// number.
pub type Location = (String, usize);

/// A `name = value` line from a profile, with includes expanded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileLine {
    /// Where it came from.
    pub location: Location,
    /// The preference name.
    pub name: String,
    /// The value, trimmed.
    pub value: String,
}

/// Errors from [`Prefs::parse_cmd_line`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CmdLineError {
    /// Bad syntax: OCaml prints this with the usage and exits with code 2.
    Usage(UsageError),
    /// A rejected value, or a failure reading an included file.
    Fatal(Error),
}

impl std::fmt::Display for CmdLineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CmdLineError::Usage(e) => e.fmt(f),
            CmdLineError::Fatal(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for CmdLineError {}

/// The RPC versions whose preference dump format is implemented.
const RPC_VERSION_1: u32 = 1;

static SOURCE: PrefDef<()> = PrefDef::new(
    "source",
    Category::Advanced(Topic::General),
    || (),
    "include a file's preferences",
    "Include preferences from a file.  \\texttt{source \\ARG{name}} reads the \
     file \\showtt{name} in the \\texttt{.unison} directory and includes its \
     contents as if it was part of a profile or given directly on command line.",
)
.cli_only();

static INCLUDE: PrefDef<()> = PrefDef::new(
    "include",
    Category::Advanced(Topic::General),
    || (),
    "include a profile's preferences",
    "Include preferences from a profile.  \\texttt{include \\ARG{name}} reads \
     the profile \\showtt{name} (or file \\showtt{name} in the \\texttt{.unison} \
     directory if profile \\showtt{name} does not exist) and includes its \
     contents as if it was part of a profile or given directly on command line.",
)
.cli_only();

/// `addprefsto`: the file that new preferences are appended to.
pub static ADD_PREFS_TO: PrefDef<String> = PrefDef::new(
    "addprefsto",
    Category::Advanced(Topic::General),
    String::new,
    "file to add new prefs to",
    "By default, new preferences added by Unison (e.g., new \\verb|ignore| \
     clauses) will be appended to whatever preference file Unison was told \
     to load at the beginning of the run.  Setting the preference \
     \\texttt{addprefsto \\ARG{filename}} makes Unison \
     add new preferences to the file named \\ARG{filename} instead.",
);

/// The registered preferences and their current values.
pub struct Prefs {
    cells: Vec<Cell>,
    /// Every accepted name, aliases included, to its cell.
    by_name: HashMap<String, usize>,
    aliases: HashMap<String, String>,
    ignored: Vec<String>,
    unison_dir: Option<PathBuf>,
    profile_name: Option<String>,
    profile_files: Vec<(PathBuf, Option<SystemTime>, u64)>,
    warnings: Vec<String>,
}

impl std::fmt::Debug for Prefs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_map()
            .entries(
                self.cells
                    .iter()
                    .map(|c| (c.def.name(), c.def.print(&c.value))),
            )
            .finish()
    }
}

impl Default for Prefs {
    fn default() -> Self {
        Self::new()
    }
}

impl Prefs {
    /// A registry with prefs.ml's own preferences (`source`, `include`,
    /// `addprefsto`).
    pub fn new() -> Self {
        let mut prefs = Prefs {
            cells: Vec::new(),
            by_name: HashMap::new(),
            aliases: HashMap::new(),
            ignored: Vec::new(),
            unison_dir: None,
            profile_name: None,
            profile_files: Vec::new(),
            warnings: Vec::new(),
        };
        for def in [&SOURCE as &'static dyn AnyPref, &INCLUDE, &ADD_PREFS_TO] {
            prefs
                .register_dyn(def)
                .expect("built-in preferences are unique");
        }
        prefs
    }

    /// Registers a preference, at its default value.
    pub fn register<T: PrefValue>(&mut self, def: &'static PrefDef<T>) -> Result<(), Error> {
        self.register_dyn(def)
    }

    fn register_dyn(&mut self, def: &'static dyn AnyPref) -> Result<(), Error> {
        let name = def.name();
        if self.by_name.contains_key(name) {
            return Err(Error::Fatal(format!("Preference {name} registered twice")));
        }
        self.by_name.insert(name.to_owned(), self.cells.len());
        self.cells.push(Cell {
            def,
            value: def.default_value(),
            set_in_profile: false,
            names: vec![name.to_owned()],
        });
        Ok(())
    }

    /// `Prefs.alias`: accepts `new_name` for a registered preference, on the
    /// command line, in profiles and in dumps from the client.
    pub fn alias<T: PrefValue>(
        &mut self,
        def: &'static PrefDef<T>,
        new_name: &str,
    ) -> Result<(), Error> {
        let i = self.index(def.name);
        if self.by_name.contains_key(new_name) {
            return Err(Error::Fatal(format!(
                "Preference {new_name} registered twice"
            )));
        }
        self.by_name.insert(new_name.to_owned(), i);
        self.aliases
            .insert(new_name.to_owned(), def.name.to_owned());
        self.cells[i].names.insert(0, new_name.to_owned());
        Ok(())
    }

    /// `Prefs.markRemoved`: a removed preference that a server should
    /// ignore when an old client sends it.
    pub fn mark_removed(&mut self, name: &str) {
        self.ignored.push(name.to_owned());
    }

    /// `Prefs.canonicalName`: resolves an alias.
    pub fn canonical_name<'a>(&'a self, name: &'a str) -> &'a str {
        self.aliases.get(name).map_or(name, String::as_str)
    }

    fn index(&self, name: &str) -> usize {
        *self
            .by_name
            .get(name)
            .unwrap_or_else(|| panic!("preference {name} is not registered"))
    }

    fn is_alias(&self, name: &str) -> bool {
        self.aliases.contains_key(name)
    }

    /// `Prefs.read`.
    ///
    /// # Panics
    ///
    /// If `def` isn't registered.
    pub fn read<T: PrefValue>(&self, def: &PrefDef<T>) -> &T {
        downcast(&self.cells[self.index(def.name)].value)
    }

    /// `Prefs.set`: sets the value and marks it as set by the user.
    pub fn set<T: PrefValue>(&mut self, def: &PrefDef<T>, value: T) {
        let i = self.index(def.name);
        let cell = &mut self.cells[i];
        cell.set_in_profile = true;
        cell.value = Box::new(value);
    }

    /// `Prefs.overrideDefault`: sets the value unless the user has.
    pub fn override_default<T: PrefValue>(&mut self, def: &PrefDef<T>, value: T) {
        let i = self.index(def.name);
        let cell = &mut self.cells[i];
        if !cell.set_in_profile {
            cell.value = Box::new(value);
        }
    }

    /// `Prefs.name`: every name of the preference, newest alias first.
    pub fn names<T: PrefValue>(&self, def: &PrefDef<T>) -> &[String] {
        &self.cells[self.index(def.name)].names
    }

    /// `Prefs.resetToDefaults`, before loading another profile.
    pub fn reset_to_defaults(&mut self) {
        for cell in &mut self.cells {
            cell.set_in_profile = false;
            cell.value = cell.def.default_value();
        }
        self.profile_files.clear();
    }

    /// Warnings produced while setting preferences (deprecations), oldest
    /// first. OCaml prints them with `Util.warn`.
    pub fn take_warnings(&mut self) -> Vec<String> {
        std::mem::take(&mut self.warnings)
    }

    // ---- Documentation ----

    /// `Prefs.typ`.
    pub fn kind(&self, name: &str) -> Option<Kind> {
        self.by_name.get(name).map(|&i| self.cells[i].def.kind())
    }

    fn category_of(&self, name: &str) -> Option<Category> {
        let &i = self.by_name.get(name)?;
        Some(if self.is_alias(name) {
            Category::Internal(Internal::Other)
        } else {
            self.cells[i].def.category()
        })
    }

    /// `Prefs.category`.
    pub fn category(&self, name: &str) -> Option<Category> {
        self.category_of(name)
    }

    /// `Prefs.documentation`: the one-line and full documentation, empty for
    /// internal preferences.
    pub fn documentation(&self, name: &str) -> (String, String) {
        match self.category_of(name) {
            Some(c) if !c.is_internal() => {
                let def = self.cells[self.by_name[name]].def;
                if def.deprecated() {
                    (
                        format!("(Deprecated) {}", def.doc()),
                        format!("{{\\em (Deprecated)}} {}", def.fulldoc()),
                    )
                } else {
                    (def.doc(), def.fulldoc().to_owned())
                }
            }
            _ => (String::new(), String::new()),
        }
    }

    /// `Prefs.list`: documented preference names, sorted.
    pub fn list(&self, include_cli_only: bool) -> Vec<String> {
        let mut names: Vec<String> = self
            .by_name
            .iter()
            .filter(|&(name, &i)| {
                let def = self.cells[i].def;
                (!def.cli_only() || include_cli_only)
                    && !self.category_of(name).is_some_and(Category::is_internal)
            })
            .map(|(name, _)| name.clone())
            .collect();
        names.sort();
        names
    }

    /// The current values as profile lines' right-hand sides, by name (the
    /// `printers`, as `dumpPrefsToStderr` shows them).
    pub fn printed_values(&self) -> BTreeMap<&'static str, Vec<String>> {
        self.cells
            .iter()
            .map(|c| (c.def.name(), c.def.print(&c.value)))
            .collect()
    }

    // ---- Shipping preferences to the server ----

    fn check_rpc_version(rpc_version: u32) -> Result<(), Error> {
        if rpc_version == RPC_VERSION_1 {
            Ok(())
        } else {
            Err(Error::Fatal(format!(
                "Internal error: invalid RPC version: {rpc_version}"
            )))
        }
    }

    /// `Prefs.dump`: the preferences to send to the server, in OCaml's order
    /// (most recently registered first).
    pub fn dump(&self, rpc_version: u32) -> Result<Vec<DumpedPref>, Error> {
        Self::check_rpc_version(rpc_version)?;
        Ok(self
            .cells
            .iter()
            .rev()
            .filter(|c| c.def.should_send(self))
            .map(|c| {
                (
                    c.def.name().to_owned(),
                    c.def.local(),
                    c.def.dump(&c.value, &c.names),
                )
            })
            .collect())
    }

    /// `Prefs.load`: applies a dump received from the client. Unknown
    /// preferences are an error unless marked local or removed.
    pub fn load(&mut self, dumped: &[DumpedPref], rpc_version: u32) -> Result<(), Error> {
        Self::check_rpc_version(rpc_version)?;
        for (name, optional, bytes) in dumped {
            match self.by_name.get(name.as_str()) {
                Some(&i) => {
                    let cell = &mut self.cells[i];
                    // Better for compatibility not to fail on these.
                    if cell.def.cli_only() {
                        continue;
                    }
                    cell.value = cell.def.load(bytes).map_err(|e| {
                        Error::Fatal(format!("Preference {name} could not be read: {e}"))
                    })?;
                }
                None if *optional || self.ignored.contains(name) => {}
                None => {
                    return Err(Error::Fatal(format!(
                        "Preference {name} not found: inconsistent Unison versions??"
                    )));
                }
            }
        }
        Ok(())
    }

    // ---- Setting from strings ----

    /// Applies a setting to the cell for `name` (`set cell v` through the
    /// preference's `pspec`), with the deprecation warning.
    fn apply(&mut self, name: &str, input: Input<'_>) -> Result<(), String> {
        let i = self.index(name);
        let cell = &mut self.cells[i];
        let name = cell.def.name();
        cell.value = cell.def.apply(&cell.value, input)?;
        cell.set_in_profile = true;
        if cell.def.deprecated() {
            let mut w = format!(
                "Preference \"{name}\" is deprecated!\nIt may be removed in the next \
                 release, so you should\nstop using this preference on the command line \
                 and\nin the profiles."
            );
            if cell.def.is_default(&cell.value) {
                w.push_str(
                    "\nYou will not lose out on anything; you have currently\nset this \
                     preference to its default value.",
                );
            }
            self.warnings.push(w);
        }
        Ok(())
    }

    /// `Prefs.processLines`: applies profile lines in order.
    pub fn process_lines(&mut self, lines: &[ProfileLine]) -> Result<(), Error> {
        for ProfileLine {
            location: (loc, line),
            name,
            value,
        } in lines
        {
            let not_valid = || {
                Error::Fatal(format!(
                    "{loc}, line {line}: `{name}' is not a valid option"
                ))
            };
            let illegal = |s: String| Error::Fatal(format!("{loc}, line {line}: {s}"));
            let Some(category) = self.category_of(name) else {
                return Err(not_valid());
            };
            let i = self.by_name[name.as_str()];
            if category == Category::Internal(Internal::Pseudo) {
                return Err(not_valid());
            }
            let def = self.cells[i].def;
            if def.cli_only() {
                return Err(illegal(format!(
                    "\"{name}\" is a command line-only option; it must not be present in a profile."
                )));
            }
            let input = match def.kind() {
                Kind::Bool => Input::Bool(match value.as_str() {
                    "true" => true,
                    "false" => false,
                    other => {
                        return Err(Error::Fatal(format!(
                            "{name} expects a boolean value, but \n{other} is not a boolean"
                        )));
                    }
                }),
                Kind::Int => Input::Int(int_of_string(value).ok_or_else(|| {
                    Error::Fatal(format!(
                        "{name} expects an integer value, but\n{value} is not an integer"
                    ))
                })?),
                _ => Input::Str(value),
            };
            self.apply(name, input).map_err(illegal)?;
        }
        Ok(())
    }

    // ---- Profiles ----

    /// Sets the directory profiles live in (`~/.unison`, or `$UNISON`).
    pub fn set_unison_dir(&mut self, dir: impl Into<PathBuf>) {
        self.unison_dir = Some(dir.into());
    }

    /// Sets the profile to load (`Prefs.profileName`).
    pub fn set_profile_name(&mut self, name: Option<String>) {
        self.profile_name = name;
    }

    /// `Prefs.profileName`.
    pub fn profile_name(&self) -> Option<&str> {
        self.profile_name.as_deref()
    }

    fn unison_dir(&self) -> Result<&Path, Error> {
        self.unison_dir
            .as_deref()
            .ok_or_else(|| Error::Fatal("The Unison directory has not been set".into()))
    }

    /// `Prefs.profilePathname`: `name` in the Unison directory, or
    /// `name.prf` if `add_ext` and `name` doesn't exist.
    pub fn profile_pathname(&self, name: &str, add_ext: bool) -> Result<PathBuf, Error> {
        let dir = self.unison_dir()?;
        let f = dir.join(name);
        Ok(if !add_ext || f.exists() {
            f
        } else {
            dir.join(format!("{name}.prf"))
        })
    }

    /// `Prefs.thePrefsFile`.
    pub fn the_prefs_file(&self) -> Result<PathBuf, Error> {
        match &self.profile_name {
            None => Err(Error::Transient(
                "No preference file has been specified".into(),
            )),
            Some(n) => self.profile_pathname(n, true),
        }
    }

    /// `Prefs.profileUnchanged`: whether every file read since the last
    /// reset is still a regular file with the same mtime and size.
    pub fn profile_unchanged(&self) -> bool {
        self.profile_files.iter().all(|(path, mtime, size)| {
            fs::metadata(path)
                .is_ok_and(|m| m.is_file() && m.modified().ok() == *mtime && m.len() == *size)
        })
    }

    /// `Prefs.readAFile`: the lines of a profile, with includes expanded.
    ///
    /// With `add_ext` this reads profile `filename` (see
    /// [`Prefs::profile_pathname`]); otherwise the file `filename` in the
    /// Unison directory. A missing file is an error if `fail`, else empty.
    pub fn read_a_file(
        &mut self,
        filename: &str,
        fail: bool,
        add_ext: bool,
    ) -> Result<Vec<ProfileLine>, Error> {
        let path = self.profile_pathname(filename, add_ext)?;
        let locname = if add_ext {
            format!("Profile \"{filename}\" (file \"{}\")", path.display())
        } else {
            format!("File \"{}\"", path.display())
        };
        let contents = fs::metadata(&path).and_then(|m| {
            self.profile_files
                .push((path.clone(), m.modified().ok(), m.len()));
            fs::read(&path)
        });
        let contents = match contents {
            Ok(c) => c,
            Err(_) if !fail => return Ok(Vec::new()),
            Err(_) if add_ext => {
                return Err(Error::Fatal(format!(
                    "Profile {filename} not found (looking for file {})",
                    path.display()
                )));
            }
            Err(_) => {
                return Err(Error::Fatal(format!(
                    "Preference file {} not found",
                    path.display()
                )));
            }
        };
        // `input_line`: split at LF; a final line without one still counts.
        let mut raw: Vec<&[u8]> = contents.split(|&b| b == b'\n').collect();
        if raw.last().is_some_and(|l| l.is_empty()) {
            raw.pop();
        }
        let mut lines = Vec::with_capacity(raw.len());
        for (n, line) in raw.into_iter().enumerate() {
            // Many Windows tools start UTF-8 files with a byte-order mark.
            let line = if n == 0 {
                line.strip_prefix(b"\xef\xbb\xbf").unwrap_or(line)
            } else {
                line
            };
            let line = std::str::from_utf8(line)
                .map_err(|_| Error::Fatal(format!("{locname}, line {}: not valid UTF-8", n + 1)))?;
            lines.push(((locname.clone(), n + 1), line.to_owned()));
        }
        self.parse_lines(lines)
    }

    /// `Prefs.parseLines`: comments, blank lines and include directives.
    fn parse_lines(&mut self, lines: Vec<(Location, String)>) -> Result<Vec<ProfileLine>, Error> {
        let mut out = Vec::new();
        for ((locname, line_num), the_line) in lines {
            let the_line = remove_trailing_cr(&the_line);
            let l = trim_whitespace(the_line);
            if l.is_empty() || l.starts_with('#') {
                continue;
            }
            let include = [
                ("include ", true, true),
                ("source ", true, false),
                ("include? ", false, true),
                ("source? ", false, false),
            ]
            .into_iter()
            .find(|(prefix, ..)| the_line.starts_with(prefix));
            if let Some((_, fail, add_ext)) = include {
                let words = split_into_words(the_line, ' ');
                let [_, f] = &words[..] else {
                    return Err(Error::Fatal(format!(
                        "{locname}, line {line_num}:\nGarbled 'include' directive: {the_line}"
                    )));
                };
                let sublines = self.read_a_file(f, fail, add_ext).map_err(|e| match e {
                    Error::Fatal(err) => Error::Fatal(format!(
                        "Included from {}, line {line_num}:\n{err}",
                        uncapitalize(&locname)
                    )),
                    other => other,
                })?;
                out.extend(sublines);
                continue;
            }
            match split_at_char(the_line, '=') {
                (name, Some(value)) => out.push(ProfileLine {
                    location: (locname, line_num),
                    name: trim_whitespace(name).to_owned(),
                    value: trim_whitespace(value).to_owned(),
                }),
                (_, None) => {
                    return Err(Error::Fatal(format!(
                        "{locname}, line {line_num}:\nGarbled line (no '='): {the_line}"
                    )));
                }
            }
        }
        Ok(out)
    }

    /// `Prefs.loadTheFile`: reads and applies the current profile, if any.
    pub fn load_the_file(&mut self) -> Result<(), Error> {
        let Some(name) = self.profile_name.clone() else {
            return Ok(());
        };
        let lines = self.read_a_file(&name, true, true)?;
        self.process_lines(&lines)
    }

    /// `Prefs.loadStrings`: applies profile-syntax lines given directly.
    pub fn load_strings(&mut self, lines: &[String]) -> Result<(), Error> {
        let lines = lines
            .iter()
            .enumerate()
            .map(|(n, l)| (("<internal preferences>".to_owned(), n + 1), l.clone()))
            .collect();
        let lines = self.parse_lines(lines)?;
        self.process_lines(&lines)
    }

    /// `Prefs.addLine`: appends a line to the profile (or to `addprefsto`).
    /// Returns the message to show; failures are reported, not raised.
    pub fn add_line(&mut self, line: &str) -> Result<String, Error> {
        let to = self.read(&ADD_PREFS_TO).clone();
        let filename = if to.is_empty() {
            self.the_prefs_file()?
        } else {
            self.profile_pathname(&to, true)?
        };
        let result = fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(&filename)
            .and_then(|mut f| write!(f, "\n{line}"));
        Ok(match result {
            Ok(()) => format!("{line}' added to profile {}", filename.display()),
            Err(e) => {
                let msg = format!("Could not write preferences file ({e})\n");
                self.warnings.push(msg.clone());
                msg
            }
        })
    }

    /// `Prefs.add`: appends `name = value` to the profile.
    pub fn add(&mut self, name: &str, value: &str) -> Result<String, Error> {
        self.add_line(&format!("{name} = {value}"))
    }

    /// `Prefs.addComment`.
    pub fn add_comment(&mut self, comment: &str) -> Result<(), Error> {
        self.add_line(&format!("# {comment}")).map(drop)
    }

    // ---- Command line ----

    fn spec(&self, keyword: &str) -> Option<Spec> {
        let name = keyword.strip_prefix('-')?;
        let category = self.category_of(name)?;
        (category != Category::Internal(Internal::Pseudo))
            .then(|| self.cells[self.by_name[name]].def.kind().spec())
    }

    /// `Prefs.parseCmdLine`: applies the command-line arguments (without the
    /// program name). Anonymous arguments go to the `rest` preference if one
    /// is registered.
    pub fn parse_cmd_line(&mut self, args: &[String]) -> Result<(), CmdLineError> {
        let has_rest = self.kind("rest").is_some_and(|k| k.spec() == Spec::String);
        let this = std::cell::RefCell::new(self);
        let fatal = std::cell::RefCell::new(None);
        let stop = |e: Error| {
            *fatal.borrow_mut() = Some(e);
            String::new()
        };
        let result = uarg::parse(
            args,
            |k| this.borrow().spec(k),
            |keyword, arg| {
                let name = &keyword[1..];
                let mut prefs = this.borrow_mut();
                let canonical = prefs.canonical_name(name).to_owned();
                let input = match &arg {
                    Arg::Bool(b) => Input::Bool(*b),
                    Arg::Int(n) => Input::Int(*n),
                    Arg::String(s) => Input::Str(s),
                    Arg::Unit | Arg::Float(_) => unreachable!("preferences take no such argument"),
                };
                match (canonical.as_str(), input) {
                    ("source" | "include", Input::Str(s)) => {
                        let lines = prefs
                            .read_a_file(s, true, canonical == "include")
                            .map_err(stop)?;
                        prefs.process_lines(&lines).map_err(stop)
                    }
                    _ => prefs.apply(name, input).map_err(|s| stop(Error::Fatal(s))),
                }
            },
            |anon| {
                if has_rest {
                    this.borrow_mut()
                        .apply("rest", Input::Str(anon))
                        .map_err(|s| stop(Error::Fatal(s)))
                } else {
                    Err("Anonymous arguments not allowed".to_owned())
                }
            },
        );
        match (result, fatal.into_inner()) {
            (_, Some(e)) => Err(CmdLineError::Fatal(e)),
            (Err(e), None) => Err(CmdLineError::Usage(e)),
            (Ok(()), None) => Ok(()),
        }
    }

    /// `Prefs.scanCmdLine`: the command-line settings by preference name,
    /// without applying them; each list has the last occurrence first.
    pub fn scan_cmd_line(
        &self,
        args: &[String],
    ) -> Result<BTreeMap<String, Vec<String>>, UsageError> {
        let has_rest = self.kind("rest").is_some_and(|k| k.spec() == Spec::String);
        let found: std::cell::RefCell<BTreeMap<String, Vec<String>>> = Default::default();
        let insert = |name: &str, value: String| {
            found
                .borrow_mut()
                .entry(name.to_owned())
                .or_default()
                .insert(0, value);
        };
        uarg::parse(
            args,
            |k| self.spec(k),
            |keyword, arg| {
                let value = match arg {
                    Arg::Bool(b) => b.to_string(),
                    Arg::Int(n) => n.to_string(),
                    Arg::String(s) => s,
                    Arg::Unit | Arg::Float(_) => unreachable!("preferences take no such argument"),
                };
                insert(&keyword[1..], value);
                Ok(())
            },
            |anon| {
                if has_rest {
                    insert("rest", anon.to_owned());
                    Ok(())
                } else {
                    Err("Anonymous arguments not allowed".to_owned())
                }
            },
        )?;
        Ok(found.into_inner())
    }
}

/// `String.uncapitalize_ascii`.
fn uncapitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_ascii_lowercase().to_string() + chars.as_str(),
        None => String::new(),
    }
}

/// `Prefs.mdumpedPrefs`: marshals a dump.
pub fn marshal_dumped(w: &mut Writer, dumped: &[DumpedPref]) {
    w.list(dumped, |w, (name, local, value)| {
        w.string(name.as_bytes());
        w.bool(*local);
        w.string(value);
    });
}

/// `Prefs.mdumpedPrefs`: unmarshals a dump.
pub fn unmarshal_dumped(r: &mut Reader<'_>) -> umarshal::Result<Vec<DumpedPref>> {
    // Preference names are ASCII identifiers; a mangled one is reported as
    // unknown by `Prefs::load`.
    r.list(|r| {
        let name = String::from_utf8_lossy(r.string_slice()?).into_owned();
        Ok((name, r.bool()?, r.string()?))
    })
}

#[cfg(test)]
mod tests;
