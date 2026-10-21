//! Aborting transfers.
//!
//! Ports Unison `src/abort.ml`.
//!
//! Derived from Unison, Copyright 1999-2020, Benjamin C. Pierce.
//!
//! OCaml keeps this state in globals of the process; here it is [`Abort`],
//! kept in a host's state slot (`Host::state::<Abort>()`). A local run's
//! client is its own host, so the user interface and the transfers share
//! it, as in OCaml; a server has its own, which nothing aborts. Whether
//! everything is aborted is also an [`AllFlag`], which the copies' blocking
//! tasks read while they run. The slot is reset in place, never replaced,
//! so a flag taken from it stays current across runs.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use gemel_base::prefs::{Category, PrefDef, Prefs, Topic};
use gemel_base::util::{Error, FileId};

/// `maxerrors`.
pub static MAX_ERRORS: PrefDef<i64> = PrefDef::new(
    "maxerrors",
    Category::Advanced(Topic::General),
    || 1,
    "maximum number of errors before a directory transfer is aborted",
    "This preference controls after how many errors Unison aborts a \
     directory transfer.  Setting it to a large number allows Unison \
     to transfer most of a directory even when some files fail to be \
     copied.  The default is 1.  If the preference is set too high, \
     Unison may take a long time to abort in case of repeated \
     failures (for instance, when the disk is full).",
);

/// Registers this module's preferences.
pub fn register_prefs(prefs: &mut Prefs) -> Result<(), Error> {
    prefs.register(&MAX_ERRORS)
}

/// The message of an item's abort.
pub const ABORTED: &str = "Aborted";

/// The message when everything is aborted.
pub const ABORTED_BY_USER: &str = "Aborted by user request";

/// Whether everything is aborted, shared with the tasks that copy off the
/// engine thread. [`Abort::reset`] clears the same flag, so one taken
/// before stays current.
#[derive(Debug, Clone, Default)]
pub struct AllFlag(Arc<AtomicBool>);

impl AllFlag {
    /// `Abort.all`, from any thread (a signal handler's, say).
    pub fn set(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    /// Whether everything is aborted.
    pub fn is_set(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }

    /// `Abort.checkAll`: fails if everything is aborted.
    pub fn check(&self) -> Result<(), Error> {
        if self.is_set() {
            return Err(Error::Transient(ABORTED_BY_USER.into()));
        }
        Ok(())
    }
}

/// Which transfers are aborted.
#[derive(Debug, Default)]
pub struct Abort {
    errors: HashMap<FileId, i64>,
    all: AllFlag,
}

impl Abort {
    /// `Abort.reset`: nothing is aborted.
    pub fn reset(&mut self) {
        self.errors.clear();
        self.all.0.store(false, Ordering::SeqCst);
    }

    /// The flag that says whether everything is aborted, for a task off the
    /// engine thread.
    pub fn all_flag(&self) -> AllFlag {
        self.all.clone()
    }

    /// `Abort.file`: aborts one item (counts an error against it).
    pub fn file(&mut self, id: FileId) {
        *self.errors.entry(id).or_insert(0) += 1;
    }

    /// `Abort.all`: aborts everything.
    pub fn all(&mut self) {
        self.all.set();
    }

    /// `Abort.isAll`.
    pub fn is_all(&self) -> bool {
        self.all.is_set()
    }

    /// `Abort.checkAll`: fails if everything is aborted.
    pub fn check_all(&self) -> Result<(), Error> {
        self.all.check()
    }

    /// `Abort.check`: fails if everything is aborted, or if the item has had
    /// `max_errors` errors (the `maxerrors` preference).
    pub fn check(&self, id: FileId, max_errors: i64) -> Result<(), Error> {
        self.check_all()?;
        if self.errors.get(&id).copied().unwrap_or(0) >= max_errors {
            return Err(Error::Transient(ABORTED.into()));
        }
        Ok(())
    }
}

/// `Abort.testException`: whether an error is an abort.
pub fn is_abort(e: &Error) -> bool {
    matches!(e, Error::Transient(m) if m == ABORTED || m == ABORTED_BY_USER)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aborting() {
        let mut a = Abort::default();
        let (x, y) = (FileId(1), FileId(2));
        assert_eq!(a.check(x, 1), Ok(()));
        a.file(x);
        let e = a.check(x, 1).unwrap_err();
        assert!(is_abort(&e));
        assert_eq!(a.check(x, 2), Ok(()));
        assert_eq!(a.check(y, 1), Ok(()));
        a.all();
        assert!(a.is_all());
        assert_eq!(
            a.check(y, 1),
            Err(Error::Transient("Aborted by user request".into()))
        );
        a.reset();
        assert_eq!(a.check(x, 1), Ok(()));
        assert!(!is_abort(&Error::Fatal(ABORTED.into())));
    }

    #[test]
    fn the_flag_is_shared_across_threads_and_resets() {
        let mut a = Abort::default();
        let flag = a.all_flag();
        let seen = std::thread::spawn({
            let flag = flag.clone();
            move || flag.check()
        });
        assert_eq!(seen.join().unwrap(), Ok(()));
        a.all();
        let seen = std::thread::spawn({
            let flag = flag.clone();
            move || flag.check()
        });
        assert!(is_abort(&seen.join().unwrap().unwrap_err()));
        // A flag taken before the reset sees it.
        a.reset();
        assert!(!flag.is_set());
    }
}
