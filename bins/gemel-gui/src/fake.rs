//! A fake source of session events, for the tests and the benchmark
//! (`examples/bench.rs`): items, update detection's result and progress
//! as the session would send them, with no session.

use gemel_ui_api::session::{ItemView, Progress, RootNames, Updates};
use gemel_ui_api::uicommon::Action;
use gemel_ui_api::{Global, Path};

/// An item as the session would show a new file at `path` in the first
/// replica, with `action`.
pub fn item(path: &str, action: Action) -> ItemView {
    ItemView {
        path: Path::<Global>::from_bytes(path.as_bytes()).unwrap(),
        path_string: path.to_owned(),
        left: "Created",
        right: "",
        action,
        conflict: matches!(action, Action::Skip(_)),
        changed_from_default: false,
        can_diff: false,
        both_files: true,
        length: 100,
        file_type: "file",
        line: String::new(),
        details: format!("new file  {path}\n"),
        problematic: matches!(action, Action::Skip(_)),
        partially_problematic: false,
        deletion: false,
        transferred: 0,
        progress: String::new(),
        outcome: None,
        status1: "new file",
        status2: "        ",
        shown_path: path.to_owned(),
        problem: None,
        skip_reason: None,
        errors1: Vec::new(),
        errors2: Vec::new(),
        props_only: false,
        favor_changed: None,
    }
}

/// `n` items, `dir/file-00000` on, one in seven a conflict (skipped), the
/// others new in the first replica.
pub fn items(n: usize) -> Vec<ItemView> {
    (0..n)
        .map(|i| {
            let action = if i % 7 == 3 {
                Action::Skip(false)
            } else {
                Action::LtoR(false)
            };
            item(&format!("dir/file-{i:05}"), action)
        })
        .collect()
}

/// Update detection's result, with `items`.
pub fn updates(items: Vec<ItemView>) -> Updates {
    let pair = |a: &str, b: &str| (a.to_owned(), b.to_owned());
    Updates {
        items,
        any_equal_updates: false,
        dangerous: Vec::new(),
        dangerous_message: None,
        found_archives: true,
        roots: RootNames {
            short: pair("a", "b"),
            narrow: pair("a", "b"),
            full: pair("a", "b"),
            headings: String::new(),
            hosts: pair("local", "local"),
        },
    }
}

/// Item `item`'s progress, `percent` of the way, in a sync of `items`
/// items of 100 bytes.
pub fn progress(item: usize, percent: u8, items: usize) -> Progress {
    let transferred = i64::from(percent.min(100));
    Progress {
        item,
        item_transferred: transferred,
        item_length: 100,
        item_progress: format!("{percent:>3}%"),
        transferred: item as i64 * 100 + transferred,
        length: items as i64 * 100,
        items_done: item,
        items,
        rate: 1_000_000.0,
        eta: "00:10".to_owned(),
    }
}
