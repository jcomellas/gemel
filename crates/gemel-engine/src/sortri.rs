//! Sorting of reconciliation items for display.
//!
//! Ports Unison `src/sortri.ml`.
//!
//! Derived from Unison, Copyright 1999-2020, Benjamin C. Pierce.
//!
//! Reconciliation sorts its items with these preferences, and the user
//! interfaces re-sort them through the sorting menu, which changes the
//! preferences for the session. The settings saved before the first
//! change are a slot of the client's [`Host`].

use std::cmp::Ordering;

use gemel_base::case;
use gemel_base::prefs::{Category, PrefDef, Prefs, Topic};
use gemel_base::util::Error;
use gemel_rpc::host::Host;

use crate::common::{self, ReconItem, Replicas, Status};
use crate::pred::Pred;

/// `sortbysize`.
pub static SORT_BY_SIZE: PrefDef<bool> = PrefDef::new(
    "sortbysize",
    Category::Advanced(Topic::Syncprocess),
    || false,
    "list changed files by size, not name",
    "When this flag is set, the user interface will list changed files \
     by size (smallest first) rather than by name.  This is useful, for \
     example, for synchronizing over slow links, since it puts very \
     large files at the end of the list where they will not prevent \
     smaller files from being transferred quickly.\n\n\
     This preference (as well as the other sorting flags, but not the \
     sorting preferences that require patterns as arguments) can be \
     set interactively and temporarily using the 'Sort' menu in the \
     graphical and text user interfaces.",
);

/// `sortnewfirst`.
pub static SORT_NEW_FIRST: PrefDef<bool> = PrefDef::new(
    "sortnewfirst",
    Category::Advanced(Topic::Syncprocess),
    || false,
    "list new before changed files",
    "When this flag is set, the user interface will list newly created \
     files before all others.  This is useful, for example, for checking \
     that newly created files are not `junk', i.e., ones that should be \
     ignored or deleted rather than synchronized.",
);

/// `sortfirst`.
pub static SORT_FIRST: Pred = Pred::new(
    "sortfirst",
    Category::Advanced(Topic::Syncprocess),
    "add a pattern to the sortfirst list",
    "Each argument to \\texttt{sortfirst} is a pattern \\ARG{pathspec}, \
     which describes a set of paths.  \
     Files matching any of these patterns will be listed first in the \
     user interface. \
     The syntax of \\ARG{pathspec} is \
     described in \\sectionref{pathspec}{Path Specification}.",
);

/// `sortlast`.
pub static SORT_LAST: Pred = Pred::new(
    "sortlast",
    Category::Advanced(Topic::Syncprocess),
    "add a pattern to the sortlast list",
    "Similar to \\verb|sortfirst|, except that files matching one of these \
     patterns will be listed at the very end.",
);

/// Registers this module's preferences, in OCaml's order.
pub fn register_prefs(prefs: &mut Prefs) -> Result<(), Error> {
    prefs.register(&SORT_BY_SIZE)?;
    prefs.register(&SORT_NEW_FIRST)?;
    SORT_FIRST.register(prefs)?;
    SORT_LAST.register(prefs)
}

/// `savedPrefs`: the sorting settings before the user first changed them.
#[derive(Debug, Default)]
struct SavedPrefs(Option<Saved>);

#[derive(Debug, Clone)]
struct Saved {
    new_first: bool,
    by_size: bool,
    first: Vec<String>,
    last: Vec<String>,
}

/// `saveSortingPrefs`: saves the settings, unless already saved.
fn save(host: &Host) {
    let saved = host.state::<SavedPrefs>();
    let mut saved = saved.borrow_mut();
    if saved.0.is_none() {
        let prefs = host.prefs().borrow();
        saved.0 = Some(Saved {
            first: SORT_FIRST.patterns(&prefs),
            last: SORT_LAST.patterns(&prefs),
            by_size: *prefs.read(&SORT_BY_SIZE),
            new_first: *prefs.read(&SORT_NEW_FIRST),
        });
    }
}

/// `restoreDefaultSettings`: the settings saved before the first change.
pub fn restore_default_settings(host: &Host) {
    let saved = host.state::<SavedPrefs>().borrow().0.clone();
    if let Some(s) = saved {
        let mut prefs = host.prefs().borrow_mut();
        prefs.set(&SORT_NEW_FIRST, s.new_first);
        prefs.set(&SORT_BY_SIZE, s.by_size);
        SORT_FIRST.intern(&mut prefs, s.first);
        SORT_LAST.intern(&mut prefs, s.last);
    }
}

/// `zeroSortingPrefs`.
fn zero(prefs: &mut Prefs) {
    prefs.set(&SORT_NEW_FIRST, false);
    prefs.set(&SORT_BY_SIZE, false);
    SORT_FIRST.intern(prefs, Vec::new());
    SORT_LAST.intern(prefs, Vec::new());
}

/// `sortByName`.
pub fn sort_by_name(host: &Host) {
    save(host);
    zero(&mut host.prefs().borrow_mut());
}

/// `sortBySize`.
pub fn sort_by_size(host: &Host) {
    save(host);
    let mut prefs = host.prefs().borrow_mut();
    zero(&mut prefs);
    prefs.set(&SORT_BY_SIZE, true);
}

/// `sortNewFirst`: toggles listing new files first.
pub fn sort_new_first(host: &Host) {
    save(host);
    let mut prefs = host.prefs().borrow_mut();
    let new_first = !*prefs.read(&SORT_NEW_FIRST);
    prefs.set(&SORT_NEW_FIRST, new_first);
}

/// `newItem`: created in either replica.
fn new_item(ri: &ReconItem) -> bool {
    match &ri.replicas {
        Replicas::Different(d) => {
            d.rc1.status == Status::Created || d.rc2.status == Status::Created
        }
        Replicas::Problem(_) => false,
    }
}

/// `compareReconItems`: the order of the items under the current
/// settings: problems, then partial problems, then `sortfirst` paths,
/// then all but `sortlast` paths, then (with `sortnewfirst`) new items,
/// then (with `sortbysize`) by size, and then by the first replica's path
/// byte by byte.
pub fn compare_recon_items(
    prefs: &Prefs,
) -> Result<impl Fn(&ReconItem, &ReconItem) -> Ordering, Error> {
    let mode = case::mode(prefs);
    let first = SORT_FIRST.compiled(prefs, mode)?;
    let last = SORT_LAST.compiled(prefs, mode)?;
    let new_first = *prefs.read(&SORT_NEW_FIRST);
    let by_size = *prefs.read(&SORT_BY_SIZE);
    Ok(move |ri1: &ReconItem, ri2: &ReconItem| {
        // Those for which `p` holds first.
        let pred = |p: &dyn Fn(&ReconItem) -> bool| p(ri2).cmp(&p(ri1));
        pred(&common::problematic)
            .then_with(|| pred(&common::partially_problematic))
            .then_with(|| pred(&|ri| first.test(ri.path1.as_bytes())))
            .then_with(|| pred(&|ri| last.test(ri.path1.as_bytes())).reverse())
            .then_with(|| {
                if new_first {
                    pred(&new_item)
                } else {
                    Ordering::Equal
                }
            })
            .then_with(|| {
                if by_size {
                    common::ri_length(ri1).cmp(&common::ri_length(ri2))
                } else {
                    Ordering::Equal
                }
            })
            .then_with(|| ri1.path1.as_bytes().cmp(ri2.path1.as_bytes()))
    })
}

/// `sortReconItems`: a stable sort by [`compare_recon_items`].
pub fn sort_recon_items(prefs: &Prefs, items: &mut [ReconItem]) -> Result<(), Error> {
    let cmp = compare_recon_items(prefs)?;
    items.sort_by(cmp);
    Ok(())
}
