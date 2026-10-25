//! What the window says once a sync is done: the status line, and the
//! summary box when the replicas are not fully synchronized.
//!
//! Ports the end of `uigtk3.ml`'s `synchronize`.

use gemel_ui_api::session::{ItemView, Summary};

/// An item the summary box lists: its path, what happened, and the errors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// The item's path.
    pub path: String,
    /// "transport failure", "conflict", ...
    pub description: String,
    /// The errors, if any.
    pub errors: Vec<String>,
}

/// What the summary box shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shown {
    /// The message.
    pub message: String,
    /// The items, under "Show details".
    pub entries: Vec<Entry>,
}

/// The status line's counts: "1 failure, 2 skipped".
pub fn status(summary: &Summary) -> String {
    let count = |n: usize, what: &str| (n > 0).then(|| format!("{n} {what}"));
    let failures = match summary.failed.len() {
        0 => None,
        1 => Some("1 failure".to_owned()),
        n => Some(format!("{n} failures")),
    };
    let parts: Vec<String> = [
        failures,
        count(summary.partial.len(), "partially transferred"),
        count(summary.skipped.len(), "skipped"),
        count(summary.not_started, "not started"),
    ]
    .into_iter()
    .flatten()
    .collect();
    format!("Synchronization complete         {}", parts.join(", "))
}

/// The summary box, if the replicas are not fully synchronized; `items`
/// are the items synced, which the summary's indices name.
pub fn shown(summary: &Summary, items: &[ItemView]) -> Option<Shown> {
    let (failures, partials, skipped, nostart) = (
        summary.failed.len(),
        summary.partial.len(),
        summary.skipped.len(),
        summary.not_started,
    );
    let total = failures + partials + skipped + nostart;
    if total == 0 {
        return None;
    }
    let format = |n: usize, item: &str, sing: &str, plur: &str| match n {
        0 => None,
        1 => Some(format!("one {item}{sing}")),
        n => Some(format!("{n} {item}{plur}")),
    };
    let infos: Vec<String> = [
        format(failures, "failure", "", "s"),
        format(partials, "partially transferred director", "y", "ies"),
        format(skipped, "skipped item", "", "s"),
        format(nostart, "not started item", "", "s"),
    ]
    .into_iter()
    .flatten()
    .collect();
    let listed = match infos.as_slice() {
        [x] => format!(" {x}"),
        l => format!(":\n  - {}", l.join(";\n  - ")),
    };
    let message = format!(
        "{}The replicas are not fully synchronized.\n{}{listed}.",
        if failures == 0 && nostart == 0 {
            "The synchronization was successful.\n\n"
        } else {
            ""
        },
        if total < 2 { "There was" } else { "There were" },
    );
    let path = |i: usize| {
        items
            .get(i)
            .map(|it| it.path_string.clone())
            .unwrap_or_default()
    };
    let mut entries = Vec::new();
    for (i, err) in &summary.failed {
        entries.push(Entry {
            path: path(*i),
            description: "transport failure".to_owned(),
            errors: vec![err.clone()],
        });
    }
    for &i in &summary.partial {
        let errors = items
            .get(i)
            .map(|it| it.errors1.iter().chain(&it.errors2).cloned().collect())
            .unwrap_or_default();
        entries.push(Entry {
            path: path(i),
            description: "partial transfer (errors during update detection)".to_owned(),
            errors,
        });
    }
    for (i, _) in &summary.skipped {
        let item = items.get(*i);
        let (description, errors) = match item.and_then(|it| it.problem.clone()) {
            Some(err) => ("error during update detection", vec![err]),
            None if item.is_some_and(|it| it.conflict) => ("conflict", Vec::new()),
            None => ("skipped", Vec::new()),
        };
        entries.push(Entry {
            path: path(*i),
            description: description.to_owned(),
            errors,
        });
    }
    Some(Shown { message, entries })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(failed: usize, partial: usize, skipped: usize, not_started: usize) -> Summary {
        Summary {
            transferred: 0,
            skipped: (0..skipped).map(|i| (i, String::new())).collect(),
            failed: (0..failed).map(|i| (i, "disk full".to_owned())).collect(),
            partial: (0..partial).collect(),
            not_started,
            aborted: not_started > 0,
            unsynchronized: Vec::new(),
        }
    }

    #[test]
    fn nothing_to_say_when_all_went_through() {
        assert_eq!(shown(&summary(0, 0, 0, 0), &[]), None);
        assert_eq!(
            status(&summary(0, 0, 0, 0)),
            "Synchronization complete         "
        );
    }

    #[test]
    fn messages_as_uigtk3_words_them() {
        let m = |f, p, s, n| shown(&summary(f, p, s, n), &[]).unwrap().message;
        assert_eq!(
            m(0, 0, 1, 0),
            "The synchronization was successful.\n\n\
             The replicas are not fully synchronized.\n\
             There was one skipped item."
        );
        assert_eq!(
            m(1, 0, 0, 0),
            "The replicas are not fully synchronized.\nThere was one failure."
        );
        assert_eq!(
            m(2, 1, 3, 0),
            "The replicas are not fully synchronized.\n\
             There were:\n  - 2 failures;\n  - one partially transferred directory;\n  - 3 skipped items."
        );
        assert_eq!(
            m(0, 2, 0, 4),
            "The replicas are not fully synchronized.\n\
             There were:\n  - 2 partially transferred directories;\n  - 4 not started items."
        );
        assert_eq!(
            status(&summary(1, 2, 3, 4)),
            "Synchronization complete         1 failure, 2 partially transferred, 3 skipped, 4 not started"
        );
    }
}
