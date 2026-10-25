//! The items' view model: the session's items, in its order, with a
//! multiple selection and what each row shows.
//!
//! Ports what `uigtk3.ml`'s main window keeps of the items (`theState`,
//! the tree view's selection), `columnsOf`, `getArrow` and
//! `nextInteresting`.
//!
//! A row's index is its item's index in the session, as in `uigtk3`,
//! where the list store and `theState` correspond one to one: sorting is
//! the session's (`Command::Sort`). The selection is a frontend concern
//! and stays here. Methods that change what is shown return the rows that
//! changed, so the Slint model can tell only those.

use gemel_ui_api::session::{ItemView, Outcome};
use gemel_ui_api::uicommon::Action;

/// What the action column shows (`getArrow`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arrow {
    /// A problem found during update detection (`failedIcon`).
    Error,
    /// Skipped (`ignoreAct`).
    Skip,
    /// From left to right (`rightArrow`), orange if partial.
    Right {
        /// Errors deep inside (`orangeRightArrow`).
        partial: bool,
    },
    /// From right to left (`leftArrow`), orange if partial.
    Left {
        /// Errors deep inside (`orangeLeftArrow`).
        partial: bool,
    },
    /// Merged (`mergeLogo`).
    Merge,
}

impl Arrow {
    /// The arrow for `action`.
    pub fn of(action: Action) -> Arrow {
        match action {
            Action::Error => Arrow::Error,
            Action::Skip(_) => Arrow::Skip,
            Action::LtoR(partial) => Arrow::Right { partial },
            Action::RtoL(partial) => Arrow::Left { partial },
            Action::Merge => Arrow::Merge,
        }
    }

    /// Words for screen readers.
    pub fn spoken(self) -> &'static str {
        match self {
            Arrow::Error => "error",
            Arrow::Skip => "skip",
            Arrow::Right { partial: false } => "left to right",
            Arrow::Right { partial: true } => "left to right, partially",
            Arrow::Left { partial: false } => "right to left",
            Arrow::Left { partial: true } => "right to left, partially",
            Arrow::Merge => "merge",
        }
    }
}

/// What the status column shows of an item's outcome (`uigtk3`'s
/// `status`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Done {
    /// Not propagated (`NoStatus`): skipped, a problem, or not yet.
    No,
    /// Propagated (`Done`).
    Yes,
    /// It failed (`Failed`).
    Failed,
}

/// A row: an item, and how far its propagation has got.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    /// The item.
    pub item: ItemView,
    /// Its progress (`c_statust`), until it is done.
    pub progress: String,
}

impl Row {
    fn new(item: ItemView) -> Row {
        Row {
            item,
            progress: String::new(),
        }
    }

    /// The action's arrow.
    pub fn arrow(&self) -> Arrow {
        Arrow::of(self.item.action)
    }

    /// `columnsOf`'s status: none for a conflict or a problem.
    pub fn done(&self) -> Done {
        if self.item.problem.is_some() || matches!(self.item.action, Action::Skip(_)) {
            return Done::No;
        }
        match self.item.outcome {
            None => Done::No,
            Some(Outcome::Succeeded) => Done::Yes,
            Some(Outcome::Failed(_)) => Done::Failed,
        }
    }

    /// The path, as the path column shows it: with a note once it has
    /// failed (`redisplay`).
    pub fn path(&self) -> String {
        match self.done() {
            Done::Failed => format!(
                "{}       [failed: click on this line for details]",
                self.item.shown_path
            ),
            _ => self.item.shown_path.clone(),
        }
    }

    /// The status column's text: the progress, until there is an outcome
    /// (`displayRowStatus`).
    pub fn status_text(&self) -> &str {
        match self.done() {
            Done::No => &self.progress,
            _ => "",
        }
    }

    /// Whether an action can be set on it: it is not a problem and hasn't
    /// been propagated (`actionPossible`).
    pub fn action_possible(&self) -> bool {
        self.item.outcome.is_none() && self.item.problem.is_none()
    }

    /// Whether there is more to show of it than its details: errors, a
    /// problem or a failure (`updateButtons`' `details`).
    pub fn has_details(&self) -> bool {
        let i = &self.item;
        !i.errors1.is_empty()
            || !i.errors2.is_empty()
            || i.problem.is_some()
            || matches!(i.outcome, Some(Outcome::Failed(_)))
    }

    /// One sentence for screen readers.
    pub fn spoken(&self) -> String {
        let side = |s: &str| {
            if s.trim().is_empty() {
                "unchanged".to_owned()
            } else {
                s.trim().to_owned()
            }
        };
        let done = match self.done() {
            Done::No if self.progress.trim().is_empty() => String::new(),
            Done::No => format!(", {}", self.progress.trim()),
            Done::Yes => ", done".to_owned(),
            Done::Failed => ", failed".to_owned(),
        };
        format!(
            "{}, left {}, {}, right {}{done}",
            self.item.shown_path,
            side(self.item.status1),
            self.arrow().spoken(),
            side(self.item.status2),
        )
    }
}

/// Modifier keys held during a click.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ClickMods {
    /// Shift: extend from the anchor.
    pub shift: bool,
    /// Cmd on macOS, Ctrl elsewhere: toggle one row.
    pub command: bool,
}

/// The rows, and the selection.
#[derive(Debug, Default)]
pub struct ReconTable {
    rows: Vec<Row>,
    selected: Vec<bool>,
    selected_count: usize,
    anchor: Option<usize>,
    cursor: Option<usize>,
}

impl ReconTable {
    /// The rows of `items`, with nothing selected.
    pub fn new(items: Vec<ItemView>) -> Self {
        let mut t = ReconTable::default();
        t.replace(items, false);
        t
    }

    /// Number of rows.
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Whether there are no rows.
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// The row `i`.
    pub fn row(&self, i: usize) -> &Row {
        &self.rows[i]
    }

    /// The rows.
    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// Whether row `i` is selected.
    pub fn is_selected(&self, i: usize) -> bool {
        self.selected[i]
    }

    /// Number of selected rows.
    pub fn selected_count(&self) -> usize {
        self.selected_count
    }

    /// The selected rows, in order.
    pub fn selected(&self) -> Vec<usize> {
        (0..self.rows.len()).filter(|&i| self.selected[i]).collect()
    }

    /// The row selected, if exactly one is (`currentRow`).
    pub fn current(&self) -> Option<usize> {
        match self.selected_count {
            1 => self.selected.iter().position(|&s| s),
            _ => None,
        }
    }

    /// The keyboard cursor, if any.
    pub fn cursor(&self) -> Option<usize> {
        self.cursor
    }

    /// Replaces the rows with `items`. With `keep`, the rows selected stay
    /// selected, where there still are as many (`displayMain`, which keeps
    /// the selected rows' indices); otherwise nothing is.
    pub fn replace(&mut self, items: Vec<ItemView>, keep: bool) {
        let n = items.len();
        self.rows = items.into_iter().map(Row::new).collect();
        if !keep {
            self.selected.clear();
            self.anchor = None;
            self.cursor = None;
        }
        self.selected.resize(n, false);
        self.selected.truncate(n);
        self.selected_count = self.selected.iter().filter(|&&s| s).count();
        self.anchor = self.anchor.filter(|&a| a < n);
        self.cursor = self.cursor.filter(|&c| c < n);
    }

    /// Item `i` has changed.
    pub fn set_item(&mut self, i: usize, item: ItemView) -> Option<usize> {
        let row = self.rows.get_mut(i)?;
        row.item = item;
        Some(i)
    }

    /// Item `i`'s progress (`updateRowStatus`).
    pub fn set_progress(&mut self, i: usize, progress: &str) -> Option<usize> {
        let row = self.rows.get_mut(i)?;
        (row.progress != progress).then(|| {
            row.progress = progress.to_owned();
            i
        })
    }

    fn set_selected(&mut self, i: usize, on: bool, changed: &mut Vec<usize>) {
        if self.selected[i] != on {
            self.selected[i] = on;
            if on {
                self.selected_count += 1;
            } else {
                self.selected_count -= 1;
            }
            changed.push(i);
        }
    }

    /// Selects only row `i`, with the cursor on it (`select`).
    pub fn select(&mut self, i: usize) -> Vec<usize> {
        let mut changed = Vec::new();
        for j in 0..self.rows.len() {
            self.set_selected(j, j == i, &mut changed);
        }
        self.anchor = Some(i);
        self.cursor = Some(i);
        changed
    }

    /// Handles a click on row `i`, as GTK's multiple selection does: a
    /// plain click selects one row, command toggles one row, shift
    /// selects from the anchor (adding to the selection if command is
    /// also held). Returns the rows whose selection changed.
    pub fn click(&mut self, i: usize, mods: ClickMods) -> Vec<usize> {
        let mut changed = Vec::new();
        match (mods.shift, mods.command, self.anchor) {
            (true, cmd, Some(anchor)) => {
                let (lo, hi) = (anchor.min(i), anchor.max(i));
                for j in 0..self.rows.len() {
                    if (lo..=hi).contains(&j) {
                        self.set_selected(j, true, &mut changed);
                    } else if !cmd {
                        self.set_selected(j, false, &mut changed);
                    }
                }
            }
            (false, true, _) => {
                let on = !self.selected[i];
                self.set_selected(i, on, &mut changed);
                self.anchor = Some(i);
            }
            _ => return self.select(i),
        }
        self.cursor = Some(i);
        changed
    }

    /// Moves the keyboard cursor by `delta` rows. With `extend`, selects
    /// from the anchor to the new cursor; otherwise only the new cursor's
    /// row. Returns the new cursor and the rows that changed.
    pub fn move_cursor(&mut self, delta: isize, extend: bool) -> (Option<usize>, Vec<usize>) {
        if self.rows.is_empty() {
            return (None, Vec::new());
        }
        let last = self.rows.len() as isize - 1;
        let target = match self.cursor {
            Some(c) => (c as isize + delta).clamp(0, last) as usize,
            None => 0,
        };
        let changed = self.click(
            target,
            ClickMods {
                shift: extend,
                command: false,
            },
        );
        (Some(target), changed)
    }

    /// Selects every row (`select_all`).
    pub fn select_all(&mut self) -> Vec<usize> {
        let mut changed = Vec::new();
        for i in 0..self.rows.len() {
            self.set_selected(i, true, &mut changed);
        }
        changed
    }

    /// Selects no row (`unselect_all`).
    pub fn deselect_all(&mut self) -> Vec<usize> {
        let mut changed = Vec::new();
        for i in 0..self.rows.len() {
            self.set_selected(i, false, &mut changed);
        }
        changed
    }

    /// `nextInteresting`: the first item after the one selected (or from
    /// the first, if not exactly one is) that isn't a problem, and, with
    /// `auto`, is a conflict.
    pub fn next_interesting(&self, auto: bool) -> Option<usize> {
        let start = self.current().map_or(0, |i| i + 1);
        (start..self.rows.len()).find(|&i| {
            let item = &self.rows[i].item;
            item.problem.is_none() && (!auto || matches!(item.action, Action::Skip(_)))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gemel_ui_api::{Global, Path};

    pub(crate) fn item(path: &str, action: Action) -> ItemView {
        ItemView {
            path: Path::<Global>::from_bytes(path.as_bytes()).unwrap(),
            path_string: path.to_owned(),
            left: "Created",
            right: "",
            action,
            conflict: matches!(action, Action::Skip(_)),
            changed_from_default: false,
            can_diff: false,
            length: 1,
            file_type: "file",
            line: String::new(),
            details: String::new(),
            problematic: false,
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

    fn table() -> ReconTable {
        let mut problem = item("p", Action::Error);
        problem.problem = Some("no".to_owned());
        ReconTable::new(vec![
            item("a", Action::LtoR(false)),
            problem,
            item("c", Action::Skip(false)),
            item("d", Action::RtoL(true)),
        ])
    }

    const SHIFT: ClickMods = ClickMods {
        shift: true,
        command: false,
    };
    const CMD: ClickMods = ClickMods {
        shift: false,
        command: true,
    };

    #[test]
    fn click_shift_and_command() {
        let mut t = table();
        t.click(0, ClickMods::default());
        assert_eq!(t.click(2, SHIFT), [1, 2]);
        assert_eq!(t.selected(), [0, 1, 2]);
        t.click(1, CMD);
        assert_eq!(t.selected(), [0, 2]);
        assert_eq!(t.current(), None);
        // The anchor moved to 1: shift+cmd adds 1..=3.
        t.click(
            3,
            ClickMods {
                shift: true,
                command: true,
            },
        );
        assert_eq!(t.selected(), [0, 1, 2, 3]);
        t.click(3, ClickMods::default());
        assert_eq!((t.selected(), t.current()), (vec![3], Some(3)));
    }

    #[test]
    fn cursor_moves_and_extends() {
        let mut t = table();
        assert_eq!(t.move_cursor(1, false).0, Some(0));
        assert_eq!(t.move_cursor(2, true).0, Some(2));
        assert_eq!(t.selected_count(), 3);
        assert_eq!(t.move_cursor(10, false).0, Some(3));
        assert_eq!(t.selected(), [3]);
        assert_eq!(t.deselect_all(), [3]);
        assert_eq!(t.select_all().len(), 4);
    }

    #[test]
    fn next_interesting_skips_problems_and_with_auto_all_but_conflicts() {
        let mut t = table();
        assert_eq!(t.next_interesting(false), Some(0));
        assert_eq!(t.next_interesting(true), Some(2));
        t.select(0);
        assert_eq!(t.next_interesting(false), Some(2));
        t.select(3);
        assert_eq!(t.next_interesting(false), None);
    }

    #[test]
    fn replacing_keeps_the_selection_or_not() {
        let mut t = table();
        t.select(3);
        t.replace(
            vec![item("a", Action::Merge), item("b", Action::Merge)],
            true,
        );
        assert_eq!((t.selected(), t.cursor()), (vec![], None));
        t.select(1);
        t.replace(
            vec![item("a", Action::Merge), item("b", Action::Merge)],
            true,
        );
        assert_eq!(t.selected(), [1]);
        t.replace(
            vec![item("a", Action::Merge), item("b", Action::Merge)],
            false,
        );
        assert!(t.selected().is_empty());
    }

    #[test]
    fn what_a_row_shows() {
        let mut r = Row::new(item("dir/f", Action::RtoL(true)));
        assert_eq!(r.arrow(), Arrow::Left { partial: true });
        r.progress = " 45%".to_owned();
        assert_eq!(r.status_text(), " 45%");
        assert_eq!(
            r.spoken(),
            "dir/f, left new file, right to left, partially, right unchanged, 45%"
        );
        r.item.outcome = Some(Outcome::Failed("disk full".to_owned()));
        assert_eq!(r.done(), Done::Failed);
        assert_eq!(r.status_text(), "");
        assert!(
            r.path()
                .ends_with("[failed: click on this line for details]")
        );
        assert!(r.has_details() && !r.action_possible());
        let skipped = Row::new(item("s", Action::Skip(false)));
        assert_eq!(skipped.done(), Done::No);
    }
}
