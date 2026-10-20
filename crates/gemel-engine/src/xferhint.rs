//! Hints for copying from existing local files.
//!
//! Ports Unison `src/xferhint.ml`.
//!
//! Derived from Unison, Copyright 1999-2020, Benjamin C. Pierce.
//!
//! Update detection records where each fingerprint was last seen on a
//! host, so that propagation can copy a file the host already has instead
//! of transferring it. The table is a [`Host`] slot.

use std::collections::HashMap;

use gemel_base::path::{Local, Path};
use gemel_base::prefs::{Category, PrefDef, Prefs, Topic};
use gemel_base::util::Error;
use gemel_fs::fspath::Fspath;
use gemel_fs::os::FullFingerprint;
use gemel_rpc::host::Host;

/// `xferbycopying`.
pub static XFER_BY_COPYING: PrefDef<bool> = PrefDef::new(
    "xferbycopying",
    Category::Advanced(Topic::Remote),
    || true,
    "optimize transfers using local copies",
    "When this preference is set, Unison will try to avoid transferring \
     file contents across the network by recognizing when a file with the \
     required contents already exists in the target replica.  This usually \
     allows file moves to be propagated very quickly.  The default value is \
     \\texttt{true}.  ",
);

/// Registers this module's preferences.
pub fn register_prefs(prefs: &mut Prefs) -> Result<(), Error> {
    prefs.register(&XFER_BY_COPYING)
}

/// Where a fingerprint was last seen (`fingerprint2pathMap`).
#[derive(Debug, Default)]
struct Hints(HashMap<FullFingerprint, (Fspath, Path<Local>)>);

/// A hint to record: the file at `path` under `fspath` has this
/// fingerprint.
pub type Entry = (Fspath, Path<Local>, FullFingerprint);

/// `insertEntry`: records a hint, unless copying is off or the fingerprint
/// is a pseudo one. A later hint for the same fingerprint replaces it.
pub fn insert_entry(host: &Host, (fspath, path, fp): Entry) {
    if *host.prefs().borrow().read(&XFER_BY_COPYING) && !fp.is_pseudo() {
        host.state::<Hints>()
            .borrow_mut()
            .0
            .insert(fp, (fspath, path));
    }
}

/// `lookup`: where a file with this fingerprint was seen.
pub fn lookup(host: &Host, fp: &FullFingerprint) -> Option<(Fspath, Path<Local>)> {
    host.state::<Hints>().borrow().0.get(fp).cloned()
}

/// `deleteEntry`.
pub fn delete_entry(host: &Host, fp: &FullFingerprint) {
    host.state::<Hints>().borrow_mut().0.remove(fp);
}
