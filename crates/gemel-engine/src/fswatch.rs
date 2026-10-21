//! Client side of the file-watcher protocol (`repeat = watch`).
//!
//! Ports Unison `src/fswatch.ml`.
//!
//! Derived from Unison, Copyright 1999-2020, Benjamin C. Pierce.
//!
//! Only the `watch` preference so far; the watcher itself is phase 9.

use gemel_base::prefs::{Category, PrefDef, Prefs, Topic};
use gemel_base::util::Error;

/// `watch` (`Fswatch.useWatcher`).
pub static USE_WATCHER: PrefDef<bool> = PrefDef::new(
    "watch",
    Category::Advanced(Topic::General),
    || false,
    "when set, use a file watcher process to detect changes",
    "Unison uses a file watcher process, when available, to detect filesystem \
     changes; this is used to speed up update detection. Setting this flag to \
     false disables the use of this process.",
);

/// Registers this module's preferences.
pub fn register_prefs(prefs: &mut Prefs) -> Result<(), Error> {
    prefs.register(&USE_WATCHER)
}
