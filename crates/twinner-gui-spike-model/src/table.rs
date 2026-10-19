//! The recon table model: rows, sort order and a multi-selection.
//!
//! Rows are addressed two ways. The *id* is the row's index in
//! [`ReconTable::rows`] and never changes. The *view index* is its position
//! in the current sort order. The selection is keyed by id, so sorting keeps
//! it. Methods that change visible state return the view indices that
//! changed, so a retained-mode toolkit (Slint) can notify only those rows.

use std::cmp::Ordering;

/// What changed on one replica since the last sync. Mirrors the status
/// column of Unison's recon table.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ReplicaStatus {
    /// Nothing changed on this side.
    Unchanged,
    /// Only properties (permissions, times) changed.
    PropsChanged,
    /// Contents changed.
    Modified,
    /// The path is new on this side.
    Created,
    /// The path was deleted on this side.
    Deleted,
}

impl ReplicaStatus {
    /// The label Unison shows in the status column.
    pub fn label(self) -> &'static str {
        match self {
            ReplicaStatus::Unchanged => "",
            ReplicaStatus::PropsChanged => "props",
            ReplicaStatus::Modified => "changed",
            ReplicaStatus::Created => "new",
            ReplicaStatus::Deleted => "deleted",
        }
    }
}

/// Which way an item will be propagated.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Direction {
    /// Unresolved conflict: both sides changed.
    Conflict,
    /// The user asked to skip this item (`/`).
    Skip,
    /// Copy left to right (`>`).
    LeftToRight,
    /// Copy right to left (`<`).
    RightToLeft,
}

impl Direction {
    /// The arrow Unison shows between the status columns.
    pub fn arrow(self) -> &'static str {
        match self {
            Direction::Conflict => "<-?->",
            Direction::Skip => "<-/->",
            Direction::LeftToRight => "---->",
            Direction::RightToLeft => "<----",
        }
    }

    /// Words for screen readers.
    pub fn spoken(self) -> &'static str {
        match self {
            Direction::Conflict => "conflict",
            Direction::Skip => "skip",
            Direction::LeftToRight => "left to right",
            Direction::RightToLeft => "right to left",
        }
    }

    /// Maps the recon-table key to a direction: `>`, `<` or `/`.
    pub fn from_key(text: &str) -> Option<Direction> {
        match text {
            ">" | "." => Some(Direction::LeftToRight),
            "<" | "," => Some(Direction::RightToLeft),
            "/" => Some(Direction::Skip),
            _ => None,
        }
    }
}

/// One recon item.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    /// Path relative to the roots.
    pub path: String,
    /// Status on the left replica.
    pub left: ReplicaStatus,
    /// Status on the right replica.
    pub right: ReplicaStatus,
    /// Propagation direction.
    pub direction: Direction,
    /// Bytes to transfer.
    pub size: u64,
    /// Transfer progress in `0.0..=1.0`.
    pub progress: f32,
}

impl Row {
    /// The size as Unison prints it in the recon table.
    pub fn size_label(&self) -> String {
        human_size(self.size)
    }

    /// One sentence for screen readers.
    pub fn spoken(&self) -> String {
        let side = |s: ReplicaStatus| {
            if s == ReplicaStatus::Unchanged {
                "unchanged"
            } else {
                s.label()
            }
        };
        format!(
            "{}, left {}, {}, right {}, {}, {:.0}% done",
            self.path,
            side(self.left),
            self.direction.spoken(),
            side(self.right),
            self.size_label(),
            self.progress * 100.0
        )
    }
}

/// Formats a byte count as `123 B`, `4.5 KiB`, ...
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = bytes as f64;
    let mut unit = 0;
    while v >= 1024.0 && unit + 1 < UNITS.len() {
        v /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{v:.1} {}", UNITS[unit])
    }
}

/// A sortable column.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortKey {
    /// By path.
    Path,
    /// Conflicts first, then by direction and the two statuses, then path
    /// (the order `sortri.ml` uses for "sort by status").
    Status,
    /// By transfer size.
    Size,
}

/// Modifier keys held during a click.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ClickMods {
    /// Shift: extend from the anchor.
    pub shift: bool,
    /// Cmd on macOS, Ctrl elsewhere: toggle one row.
    pub command: bool,
}

/// Rows, sort order and selection.
#[derive(Debug)]
pub struct ReconTable {
    rows: Vec<Row>,
    /// View index -> id.
    order: Vec<u32>,
    /// Id -> view index.
    position: Vec<u32>,
    selected: Vec<bool>,
    selected_count: usize,
    anchor: Option<u32>,
    cursor: Option<u32>,
    sort: Option<(SortKey, bool)>,
}

impl ReconTable {
    /// Wraps `rows` in their original order with nothing selected.
    pub fn new(rows: Vec<Row>) -> Self {
        let n = rows.len();
        let order: Vec<u32> = (0..n as u32).collect();
        ReconTable {
            rows,
            position: order.clone(),
            order,
            selected: vec![false; n],
            selected_count: 0,
            anchor: None,
            cursor: None,
            sort: None,
        }
    }

    /// Number of rows.
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// True if there are no rows.
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// The row shown at `view`.
    pub fn row_at(&self, view: usize) -> &Row {
        &self.rows[self.order[view] as usize]
    }

    /// The id of the row shown at `view`.
    pub fn id_at(&self, view: usize) -> u32 {
        self.order[view]
    }

    /// Where the row with `id` is shown.
    pub fn view_of(&self, id: u32) -> usize {
        self.position[id as usize] as usize
    }

    /// Whether the row shown at `view` is selected.
    pub fn is_selected(&self, view: usize) -> bool {
        self.selected[self.order[view] as usize]
    }

    /// Number of selected rows.
    pub fn selected_count(&self) -> usize {
        self.selected_count
    }

    /// View index of the keyboard cursor (the focused row), if any.
    pub fn cursor(&self) -> Option<usize> {
        self.cursor.map(|id| self.view_of(id))
    }

    /// The current sort column and whether it is ascending.
    pub fn sort(&self) -> Option<(SortKey, bool)> {
        self.sort
    }

    fn set_selected(&mut self, id: u32, on: bool, changed: &mut Vec<usize>) {
        let slot = &mut self.selected[id as usize];
        if *slot != on {
            *slot = on;
            if on {
                self.selected_count += 1;
            } else {
                self.selected_count -= 1;
            }
            changed.push(self.position[id as usize] as usize);
        }
    }

    /// Deselects every selected row whose view index is outside `lo..=hi`.
    fn retain_range(&mut self, lo: usize, hi: usize, changed: &mut Vec<usize>) {
        if self.selected_count == 0 {
            return;
        }
        for id in 0..self.rows.len() as u32 {
            let view = self.position[id as usize] as usize;
            if !(lo..=hi).contains(&view) {
                self.set_selected(id, false, changed);
            }
        }
    }

    fn select_range(&mut self, from: usize, to: usize, only: bool, changed: &mut Vec<usize>) {
        let (lo, hi) = if from <= to { (from, to) } else { (to, from) };
        if only {
            self.retain_range(lo, hi, changed);
        }
        for view in lo..=hi {
            self.set_selected(self.order[view], true, changed);
        }
    }

    /// Handles a click on the row at `view`, with Finder/Explorer semantics:
    /// plain click selects one row, command toggles one row, shift selects
    /// from the anchor (adding to the selection if command is also held).
    /// Returns the view indices whose selection state changed.
    pub fn click(&mut self, view: usize, mods: ClickMods) -> Vec<usize> {
        let mut changed = Vec::new();
        let id = self.order[view];
        match (mods.shift, mods.command, self.anchor) {
            (true, cmd, Some(anchor)) => {
                self.select_range(self.view_of(anchor), view, !cmd, &mut changed);
            }
            (false, true, _) => {
                let on = !self.selected[id as usize];
                self.set_selected(id, on, &mut changed);
                self.anchor = Some(id);
            }
            _ => {
                self.select_range(view, view, true, &mut changed);
                self.anchor = Some(id);
            }
        }
        self.cursor = Some(id);
        changed
    }

    /// Moves the keyboard cursor by `delta` rows. With `extend`, selects from
    /// the anchor to the new cursor (shift+arrow); otherwise selects only the
    /// new cursor row. Returns the new cursor view index and the changed
    /// view indices.
    pub fn move_cursor(&mut self, delta: isize, extend: bool) -> (Option<usize>, Vec<usize>) {
        if self.rows.is_empty() {
            return (None, Vec::new());
        }
        let last = self.rows.len() as isize - 1;
        let target = match self.cursor() {
            Some(view) => (view as isize + delta).clamp(0, last) as usize,
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

    /// Selects every row. Returns the changed view indices.
    pub fn select_all(&mut self) -> Vec<usize> {
        let mut changed = Vec::new();
        for id in 0..self.rows.len() as u32 {
            self.set_selected(id, true, &mut changed);
        }
        changed
    }

    /// Sets the direction of every selected row, as the `>` `<` `/` keys do.
    /// Returns the view indices that changed.
    pub fn set_direction(&mut self, direction: Direction) -> Vec<usize> {
        let mut changed = Vec::new();
        for (id, row) in self.rows.iter_mut().enumerate() {
            if self.selected[id] && row.direction != direction {
                row.direction = direction;
                changed.push(self.position[id] as usize);
            }
        }
        changed
    }

    /// Applies progress updates from the engine. Returns the view indices
    /// that changed.
    pub fn apply_progress(&mut self, updates: &[ProgressUpdate]) -> Vec<usize> {
        updates
            .iter()
            .map(|u| {
                self.rows[u.id as usize].progress = u.progress;
                self.position[u.id as usize] as usize
            })
            .collect()
    }

    /// Sorts by `key`. Sorting by the current key again flips the order.
    pub fn sort_by(&mut self, key: SortKey) {
        let ascending = match self.sort {
            Some((k, asc)) if k == key => !asc,
            _ => true,
        };
        self.sort_with(key, ascending);
    }

    /// Sorts by `key` in the given order.
    pub fn sort_with(&mut self, key: SortKey, ascending: bool) {
        let rows = &self.rows;
        let cmp = |a: &u32, b: &u32| -> Ordering {
            let (ra, rb) = (&rows[*a as usize], &rows[*b as usize]);
            let ord = match key {
                SortKey::Path => Ordering::Equal,
                SortKey::Status => {
                    (ra.direction, ra.left, ra.right).cmp(&(rb.direction, rb.left, rb.right))
                }
                SortKey::Size => ra.size.cmp(&rb.size),
            }
            .then_with(|| ra.path.cmp(&rb.path));
            if ascending { ord } else { ord.reverse() }
        };
        self.order.sort_by(cmp);
        for (view, &id) in self.order.iter().enumerate() {
            self.position[id as usize] = view as u32;
        }
        self.sort = Some((key, ascending));
    }
}

/// One progress update from the (fake) engine.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ProgressUpdate {
    /// Row id.
    pub id: u32,
    /// New progress in `0.0..=1.0`.
    pub progress: f32,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table() -> ReconTable {
        let row = |path: &str, direction, size| Row {
            path: path.to_string(),
            left: ReplicaStatus::Modified,
            right: ReplicaStatus::Unchanged,
            direction,
            size,
            progress: 0.0,
        };
        ReconTable::new(vec![
            row("c", Direction::LeftToRight, 1),
            row("a", Direction::Conflict, 3),
            row("d", Direction::RightToLeft, 2),
            row("b", Direction::Conflict, 4),
        ])
    }

    fn paths(t: &ReconTable) -> Vec<&str> {
        (0..t.len()).map(|v| t.row_at(v).path.as_str()).collect()
    }

    fn selected_paths(t: &ReconTable) -> Vec<&str> {
        (0..t.len())
            .filter(|&v| t.is_selected(v))
            .map(|v| t.row_at(v).path.as_str())
            .collect()
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
    fn sorts_and_toggles_order() {
        let mut t = table();
        t.sort_by(SortKey::Path);
        assert_eq!(paths(&t), ["a", "b", "c", "d"]);
        t.sort_by(SortKey::Path);
        assert_eq!(paths(&t), ["d", "c", "b", "a"]);
        t.sort_by(SortKey::Status);
        assert_eq!(paths(&t), ["a", "b", "c", "d"]);
        assert_eq!(t.sort(), Some((SortKey::Status, true)));
        t.sort_by(SortKey::Size);
        assert_eq!(paths(&t), ["c", "d", "a", "b"]);
    }

    #[test]
    fn click_shift_and_command() {
        let mut t = table();
        t.sort_by(SortKey::Path);
        t.click(0, ClickMods::default());
        let changed = t.click(2, SHIFT);
        assert_eq!(selected_paths(&t), ["a", "b", "c"]);
        assert_eq!(changed, [1, 2]);
        t.click(1, CMD);
        assert_eq!(selected_paths(&t), ["a", "c"]);
        // The anchor moved to "b": shift+cmd adds b..d to the selection.
        t.click(
            3,
            ClickMods {
                shift: true,
                command: true,
            },
        );
        assert_eq!(selected_paths(&t), ["a", "b", "c", "d"]);
        t.click(3, ClickMods::default());
        assert_eq!(selected_paths(&t), ["d"]);
        assert_eq!(t.selected_count(), 1);
    }

    #[test]
    fn selection_survives_sorting() {
        let mut t = table();
        t.sort_by(SortKey::Path);
        t.click(0, ClickMods::default());
        t.click(1, SHIFT);
        t.sort_by(SortKey::Path);
        assert_eq!(selected_paths(&t), ["b", "a"]);
        assert_eq!(t.cursor(), Some(2));
    }

    #[test]
    fn direction_keys_apply_to_selection() {
        let mut t = table();
        t.sort_by(SortKey::Path);
        t.click(0, ClickMods::default());
        t.click(1, SHIFT);
        let changed = t.set_direction(Direction::from_key(">").unwrap());
        assert_eq!(changed.len(), 2);
        assert!((0..2).all(|v| t.row_at(v).direction == Direction::LeftToRight));
        assert_eq!(t.row_at(3).direction, Direction::RightToLeft);
        assert!(t.set_direction(Direction::LeftToRight).is_empty());
    }

    #[test]
    fn cursor_moves_and_extends() {
        let mut t = table();
        let (c, _) = t.move_cursor(1, false);
        assert_eq!(c, Some(0));
        let (c, _) = t.move_cursor(2, true);
        assert_eq!(c, Some(2));
        assert_eq!(t.selected_count(), 3);
        let (c, _) = t.move_cursor(10, false);
        assert_eq!(c, Some(3));
        assert_eq!(t.selected_count(), 1);
    }

    #[test]
    fn progress_reports_view_index() {
        let mut t = table();
        t.sort_by(SortKey::Path);
        let changed = t.apply_progress(&[ProgressUpdate {
            id: 0,
            progress: 0.5,
        }]);
        assert_eq!(changed, [2]);
        assert_eq!(t.row_at(2).progress, 0.5);
    }

    #[test]
    fn sizes() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(1536), "1.5 KiB");
        assert_eq!(human_size(5 * 1024 * 1024), "5.0 MiB");
    }
}
