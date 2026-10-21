//! Going through the items: `interact`, with its keys, its matching
//! conditions, and the actions on the current item or the matching ones.
//!
//! Derived from Unison, Copyright 1999-2020, Benjamin C. Pierce.
//!
//! As in OCaml the list is a zipper: `prev`, the items before the current
//! one (the last is the nearest), and `ril`, the current one and those
//! after it.

use std::collections::{HashMap, VecDeque};

use gemel_base::util::MY_NAME;
use gemel_ui_api::session::{Command, Event, IgnoreKind, ItemView, SetAction};
use gemel_ui_api::uicommon::Action;

use super::{Color, Stop, Text};
use crate::infos;
use crate::input::{
    KEY_DELETE, KEY_DN, KEY_END, KEY_ESC, KEY_HOME, KEY_LEFT, KEY_PG_DN, KEY_PG_UP, KEY_RIGHT,
    KEY_RV_TAB, KEY_TAB, KEY_UP,
};

/// An item: its number in the session, and how it is now.
#[derive(Clone)]
pub(super) struct Entry {
    pub i: usize,
    pub v: ItemView,
}

/// `proceed`: what to do once through the items.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Proceed {
    /// `ConfirmBeforeProceeding`.
    ConfirmBefore,
    /// `ProceedImmediately`.
    Immediately,
}

/// A choice for `selectAction`: its keys (a first `""` makes it the
/// default, named by the second), what it does, and the answer.
pub(super) struct Choice<A> {
    pub names: Vec<String>,
    pub doc: String,
    pub act: A,
}

impl<A> Choice<A> {
    pub fn new(names: &[&str], doc: impl Into<String>, act: A) -> Choice<A> {
        Choice {
            names: names.iter().map(|n| (*n).to_owned()).collect(),
            doc: doc.into(),
            act,
        }
    }
}

/// A matching condition.
#[derive(Clone)]
enum Pred {
    All,
    LtoR,
    RtoL,
    Conflict,
    /// `ispropschanged`.
    PropsOnly,
    Merge,
    Not(Box<Pred>),
    And(Box<Pred>, Box<Pred>),
    Or(Box<Pred>, Box<Pred>),
}

impl Pred {
    fn test(&self, v: &ItemView) -> bool {
        match self {
            Pred::All => true,
            Pred::LtoR => matches!(v.action, Action::LtoR(_)),
            Pred::RtoL => matches!(v.action, Action::RtoL(_)),
            Pred::Conflict => matches!(v.action, Action::Skip(_)),
            Pred::PropsOnly => v.props_only,
            Pred::Merge => v.action == Action::Merge,
            Pred::Not(p) => !p.test(v),
            Pred::And(p, q) => p.test(v) && q.test(v),
            Pred::Or(p, q) => p.test(v) || q.test(v),
        }
    }
}

/// What `setripred` does to the conditions.
enum PredCmd {
    Unset,
    Pop,
    Push(Pred),
    Not,
    And,
    Or,
}

/// What `actOnMatching` does to an item.
#[derive(Clone, Copy)]
enum Op {
    /// `showdiffs`.
    Diff,
    /// `displayDetails`.
    Details,
    Set(SetAction),
    /// `setdirchanged`.
    FavorChanged,
    /// `invertdir`.
    Invert,
    /// `setskip`.
    Skip,
    /// Drops it.
    Discard,
}

/// When an [`Op`] can't be done: nothing, a message, or (`None` in OCaml)
/// the item is dropped.
#[derive(Clone, Copy)]
enum Fail {
    Quiet,
    Say(&'static str),
    Discard,
}

/// The keys of `interact`.
#[derive(Clone, Copy)]
enum Key {
    Follow,
    Next,
    Previous,
    RevertPrevious,
    Start,
    End,
    Forward,
    Backward,
    Reverse,
    Diff,
    Details,
    ListTerse,
    List,
    MatchAll,
    MatchLtoR,
    MatchRtoL,
    MatchConflicts,
    MatchProps,
    MatchMerges,
    MatchNot,
    MatchAnd,
    MatchOr,
    MatchPop,
    Unmatch,
    Revert,
    Merge,
    Right,
    Left,
    Newer,
    Older,
    Changed,
    Invert,
    Skip,
    SkipAll,
    Discard,
    DiscardAll,
    IgnorePath,
    IgnoreExt,
    IgnoreName,
    Stop,
    Go,
    Quit,
}

/// The choices for an item: a conflict has no default.
fn choices(conflict: bool, descr: &str, descl: &str) -> Vec<Choice<Key>> {
    use Key::*;
    let c = Choice::new;
    vec![
        c(
            if conflict { &["f"] } else { &["", "f", " "] },
            format!("follow {MY_NAME}'s recommendation (if any)"),
            Follow,
        ),
        c(
            &["n", "j", KEY_DN, KEY_TAB],
            "go to the next item".into(),
            Next,
        ),
        c(
            &["p", "b", "k", KEY_UP, KEY_RV_TAB],
            "go back to previous item".into(),
            Previous,
        ),
        c(
            &["\x7f", "\x08", KEY_DELETE],
            "revert then go back to previous item".into(),
            RevertPrevious,
        ),
        c(
            &["0", KEY_HOME],
            "go to the start of the list".into(),
            Start,
        ),
        c(&["9", KEY_END], "go to the end of the list".into(), End),
        c(
            &["5", KEY_PG_DN],
            "go forward to the middle of the following items".into(),
            Forward,
        ),
        c(
            &["6", KEY_PG_UP],
            "go backward to the middle of the preceding items".into(),
            Backward,
        ),
        c(&["R"], "reverse the list of paths".into(), Reverse),
        c(&["d"], "show differences (curr or match)".into(), Diff),
        c(&["x"], "show details (curr or match)".into(), Details),
        c(
            &["L"],
            "list all (or matching) following changes tersely".into(),
            ListTerse,
        ),
        c(
            &["l"],
            "list all (or matching) following changes with details".into(),
            List,
        ),
        c(&["A", "*"], "match all the following".into(), MatchAll),
        c(
            &["1"],
            format!("match all the following that propagate {descr}"),
            MatchLtoR,
        ),
        c(
            &["2"],
            format!("match all the following that propagate {descl}"),
            MatchRtoL,
        ),
        c(
            &["C"],
            "match all the following conflicts".into(),
            MatchConflicts,
        ),
        c(
            &["P", "="],
            "match all the following with only props changes".into(),
            MatchProps,
        ),
        c(&["M"], "match all the following merges".into(), MatchMerges),
        c(
            &["X", "!"],
            "invert the matching condition".into(),
            MatchNot,
        ),
        c(
            &["&"],
            "and the last two matching conditions".into(),
            MatchAnd,
        ),
        c(
            &["|"],
            "or the last two matching conditions".into(),
            MatchOr,
        ),
        c(
            &["D", "_"],
            "delete/pop the active matching condition".into(),
            MatchPop,
        ),
        c(&["U", "$"], "unmatch (select current)".into(), Unmatch),
        c(
            &["r", "u"],
            format!("revert to {MY_NAME}'s default recommendation (curr or match)"),
            Revert,
        ),
        c(&["m"], "merge the versions (curr or match)".into(), Merge),
        c(
            &[">", ".", KEY_RIGHT],
            format!("propagate from {descr} (curr or match)"),
            Right,
        ),
        c(
            &["<", ",", KEY_LEFT],
            format!("propagate from {descl} (curr or match)"),
            Left,
        ),
        c(
            &["]", "\""],
            "resolve conflicts in favor of the newer (curr or match)".into(),
            Newer,
        ),
        c(
            &["[", "'"],
            "resolve conflicts in favor of the older (curr or match)".into(),
            Older,
        ),
        c(
            &["c"],
            "resolve conflicts in favor of changed (curr or match)".into(),
            Changed,
        ),
        c(
            &["i"],
            "invert direction of propagation (curr or match)".into(),
            Invert,
        ),
        c(&["/", ":"], "skip (curr or match)".into(), Skip),
        c(&["%"], "skip all the following".into(), SkipAll),
        c(
            &["-"],
            "skip and discard for this session (curr or match)".into(),
            Discard,
        ),
        c(
            &["+"],
            "skip and discard all the following".into(),
            DiscardAll,
        ),
        c(&["I"], "ignore this path permanently".into(), IgnorePath),
        c(
            &["E"],
            "permanently ignore files with this extension".into(),
            IgnoreExt,
        ),
        c(
            &["N"],
            "permanently ignore paths ending with this name".into(),
            IgnoreName,
        ),
        c(
            &["s"],
            "stop reconciling and go to the proceed menu".into(),
            Stop,
        ),
        c(
            &["g"],
            "proceed immediately to propagating changes".into(),
            Go,
        ),
        c(
            &["q", KEY_ESC],
            format!("exit {MY_NAME} without propagating any changes"),
            Quit,
        ),
    ]
}

impl Text {
    fn headings(&self) -> String {
        self.roots
            .as_ref()
            .map_or_else(String::new, |r| r.headings.clone())
    }

    /// `interact`: goes through the items from `ril`'s first, asking what
    /// to do with each (in batch mode, listing them); what to do next,
    /// and the items, in their order.
    pub(super) fn interact(
        &mut self,
        mut prev: Vec<Entry>,
        mut ril: VecDeque<Entry>,
    ) -> Result<(Proceed, Vec<Entry>), Stop> {
        let batch = self.batch();
        if !batch {
            self.display(&format!("\n{}\n", self.headings()));
        }
        let (host1, host2) = self
            .roots
            .as_ref()
            .map(|r| r.hosts.clone())
            .unwrap_or_default();
        let (descr, descl) = if host1 == host2 {
            ("left to right".to_owned(), "right to left".to_owned())
        } else {
            (
                format!("from {host1} to {host2}"),
                format!("from {host2} to {host1}"),
            )
        };
        let mut ripred: Vec<Pred> = Vec::new();
        loop {
            let Some(ri) = ril.front().cloned() else {
                return Ok((Proceed::ConfirmBefore, prev));
            };
            let next = |prev: &mut Vec<Entry>, ril: &mut VecDeque<Entry>| {
                prev.extend(ril.pop_front());
            };
            self.display_ri(&ri.v);
            if let Some(p) = &ri.v.problem {
                infos::print(&format!("\n{p}\n"));
                next(&mut prev, &mut ril);
                continue;
            }
            let conflict = matches!(ri.v.action, Action::Skip(_));
            if self.settings().auto && !conflict {
                self.display("\n");
                next(&mut prev, &mut ril);
                continue;
            }
            if batch {
                if self.silent() && conflict {
                    infos::print("\n");
                }
                self.display("\n");
                if !self.terse() {
                    self.display_details(&ri.v);
                }
                next(&mut prev, &mut ril);
                continue;
            }
            let cs = choices(conflict, &descr, &descl);
            let shown = ri.v.clone();
            let key = self.select_action(None, &cs, &move |t| t.display_ri(&shown))?;
            match key {
                Key::Follow => {
                    self.new_line();
                    if conflict {
                        self.display("No default action [type '?' for help]\n");
                    } else {
                        next(&mut prev, &mut ril);
                    }
                }
                Key::Next => {
                    self.new_line();
                    next(&mut prev, &mut ril);
                }
                Key::Previous => {
                    self.new_line();
                    self.previous(&mut prev, &mut ril);
                }
                Key::RevertPrevious => {
                    if let Some(mut e) = ril.pop_front() {
                        self.set_action(&mut e, SetAction::Revert)?;
                        self.redisplay(&e.v);
                        ril.push_front(e);
                    }
                    self.previous(&mut prev, &mut ril);
                }
                Key::Start => {
                    self.new_line();
                    let mut all: VecDeque<Entry> = std::mem::take(&mut prev).into();
                    all.extend(ril.drain(..));
                    ril = all;
                }
                Key::End => {
                    self.new_line();
                    let n = ril.len();
                    prev.extend(ril.drain(..n - 1));
                }
                Key::Forward => {
                    self.new_line();
                    let l = (ril.len() / 2) as i64;
                    self.display(&format!("  Moving {l} items forward\n"));
                    forward(l, &mut prev, &mut ril);
                }
                Key::Backward => {
                    self.new_line();
                    let l = -(prev.len().div_ceil(2) as i64);
                    self.display(&format!("  Moving {l} items backward\n"));
                    forward(l, &mut prev, &mut ril);
                }
                Key::Reverse => {
                    self.new_line();
                    let ri = ril.pop_front();
                    let rest: Vec<Entry> = ril.drain(..).rev().collect();
                    ril = ri.into_iter().chain(prev.drain(..).rev()).collect();
                    prev = rest;
                }
                Key::Diff => self.act_on_matching(
                    &mut prev,
                    &mut ril,
                    &ripred,
                    false,
                    Fail::Quiet,
                    Op::Diff,
                )?,
                Key::Details => self.act_on_matching(
                    &mut prev,
                    &mut ril,
                    &ripred,
                    false,
                    Fail::Quiet,
                    Op::Details,
                )?,
                Key::ListTerse | Key::List => {
                    self.new_line();
                    for e in ril.iter().filter(|e| ritest(&ripred, &e.v)) {
                        self.display("  ");
                        self.display_ri(&e.v);
                        self.display("\n");
                        if matches!(key, Key::List) {
                            self.always_display_details(&e.v);
                        }
                    }
                }
                Key::MatchAll => self.set_ripred(&mut ripred, PredCmd::Push(Pred::All)),
                Key::MatchLtoR => self.set_ripred(&mut ripred, PredCmd::Push(Pred::LtoR)),
                Key::MatchRtoL => self.set_ripred(&mut ripred, PredCmd::Push(Pred::RtoL)),
                Key::MatchConflicts => self.set_ripred(&mut ripred, PredCmd::Push(Pred::Conflict)),
                Key::MatchProps => self.set_ripred(&mut ripred, PredCmd::Push(Pred::PropsOnly)),
                Key::MatchMerges => self.set_ripred(&mut ripred, PredCmd::Push(Pred::Merge)),
                Key::MatchNot => self.set_ripred(&mut ripred, PredCmd::Not),
                Key::MatchAnd => self.set_ripred(&mut ripred, PredCmd::And),
                Key::MatchOr => self.set_ripred(&mut ripred, PredCmd::Or),
                Key::MatchPop => self.set_ripred(&mut ripred, PredCmd::Pop),
                Key::Unmatch => self.set_ripred(&mut ripred, PredCmd::Unset),
                Key::Revert
                | Key::Merge
                | Key::Right
                | Key::Left
                | Key::Newer
                | Key::Older
                | Key::Changed
                | Key::Invert
                | Key::Skip
                | Key::Discard => {
                    let (op, fail) = match key {
                        Key::Revert => (Op::Set(SetAction::Revert), Fail::Quiet),
                        Key::Merge => (Op::Set(SetAction::Merge), Fail::Quiet),
                        Key::Right => (Op::Set(SetAction::Right), Fail::Quiet),
                        Key::Left => (Op::Set(SetAction::Left), Fail::Quiet),
                        Key::Newer => (Op::Set(SetAction::NewerIfConflict), Fail::Quiet),
                        Key::Older => (Op::Set(SetAction::OlderIfConflict), Fail::Quiet),
                        Key::Changed => (Op::FavorChanged, Fail::Say("Cannot set direction\n")),
                        Key::Invert => (Op::Invert, Fail::Say("Cannot invert direction\n")),
                        Key::Skip => (Op::Skip, Fail::Quiet),
                        _ => (Op::Discard, Fail::Discard),
                    };
                    self.act_on_matching(&mut prev, &mut ril, &ripred, true, fail, op)?;
                }
                Key::SkipAll => {
                    self.new_line();
                    let mut rest: Vec<Entry> = ril.drain(1..).collect();
                    for e in &mut rest {
                        self.apply(Op::Skip, e)?;
                    }
                    ril.extend(rest);
                }
                Key::DiscardAll => {
                    self.new_line();
                    ril.truncate(1);
                }
                Key::IgnorePath => {
                    self.new_line();
                    self.ignore_pref(&mut prev, &mut ril, IgnoreKind::Path, "this path")?;
                }
                Key::IgnoreExt => {
                    self.new_line();
                    self.ignore_pref(
                        &mut prev,
                        &mut ril,
                        IgnoreKind::Ext,
                        "files with this extension",
                    )?;
                }
                Key::IgnoreName => {
                    self.new_line();
                    self.ignore_pref(
                        &mut prev,
                        &mut ril,
                        IgnoreKind::Name,
                        "files with this name",
                    )?;
                }
                Key::Stop | Key::Go => {
                    self.new_line();
                    prev.extend(ril);
                    let p = if matches!(key, Key::Go) {
                        Proceed::Immediately
                    } else {
                        Proceed::ConfirmBefore
                    };
                    return Ok((p, prev));
                }
                Key::Quit => {
                    self.new_line();
                    return Err(self.brk());
                }
            }
        }
    }

    /// `redisplayri`: the item again, over itself in raw mode.
    fn redisplay(&self, v: &ItemView) {
        self.overwrite();
        self.display_ri(v);
        self.display("\n");
    }

    /// `previous`: back to the item before, showing the problems on the
    /// way; before the first, the headings again.
    fn previous(&self, prev: &mut Vec<Entry>, ril: &mut VecDeque<Entry>) {
        loop {
            match prev.pop() {
                Some(e) => {
                    let problem = e.v.problem.clone();
                    if let Some(p) = &problem {
                        self.display_ri(&e.v);
                        self.display("\n");
                        self.display(p);
                        self.display("\n");
                    }
                    ril.push_front(e);
                    if problem.is_none() {
                        return;
                    }
                }
                None => {
                    self.display(&format!("\n{}\n", self.headings()));
                    return;
                }
            }
        }
    }

    /// `setripred`.
    fn set_ripred(&self, ripred: &mut Vec<Pred>, cmd: PredCmd) {
        self.new_line();
        match (cmd, ripred.len()) {
            (PredCmd::Unset, 0) => self.display("Matching condition already disabled\n"),
            (PredCmd::Unset, _) | (PredCmd::Pop, 1) => {
                self.display("  Disabling matching condition\n");
                ripred.clear();
            }
            (PredCmd::Pop, n) if n > 1 => {
                ripred.pop();
            }
            (PredCmd::Push(p), 0) => {
                self.display("  Enabling matching condition\n");
                ripred.push(p);
            }
            (PredCmd::Push(p), _) => ripred.push(p),
            (_, 0) => self.display("Matching condition not enabled\n"),
            (PredCmd::Not, _) => {
                let p = ripred.pop().expect("a condition");
                ripred.push(Pred::Not(Box::new(p)));
            }
            (PredCmd::And | PredCmd::Or, 1) => {
                self.display("Missing previous matching condition\n");
            }
            (cmd, _) => {
                let p = Box::new(ripred.pop().expect("a condition"));
                let pp = Box::new(ripred.pop().expect("a condition"));
                ripred.push(match cmd {
                    PredCmd::And => Pred::And(p, pp),
                    _ => Pred::Or(p, pp),
                });
            }
        }
    }

    /// `actOnMatching`: `op` on the current item, or with a matching
    /// condition on the following items that match (dropping those it
    /// can't be done to, with [`Fail::Discard`]). With `change`, the item
    /// is shown again and the next comes; otherwise `op` prints something
    /// on its own lines and the item stays.
    fn act_on_matching(
        &mut self,
        prev: &mut Vec<Entry>,
        ril: &mut VecDeque<Entry>,
        ripred: &[Pred],
        change: bool,
        fail: Fail,
        op: Op,
    ) -> Result<(), Stop> {
        let discard = matches!(fail, Fail::Discard);
        let Some(mut ri) = ril.pop_front() else {
            return Ok(());
        };
        match ripred.last() {
            None => {
                if !change {
                    self.new_line();
                }
                let t = self.apply(op, &mut ri)?;
                if t || !discard {
                    if change {
                        self.redisplay(&ri.v);
                    }
                    if let (false, Fail::Say(m)) = (t, fail) {
                        self.display(m);
                    }
                    if t && change {
                        prev.push(ri);
                    } else {
                        ril.push_front(ri);
                    }
                } else if change {
                    self.new_line();
                }
            }
            Some(test) => {
                self.new_line();
                let rest: Vec<Entry> = ril.drain(..).collect();
                ril.push_back(ri);
                for mut e in rest {
                    if !test.test(&e.v) || self.apply(op, &mut e)? || !discard {
                        ril.push_back(e);
                    }
                }
            }
        }
        Ok(())
    }

    /// Does `op` to `e`: false if it can't be done.
    fn apply(&mut self, op: Op, e: &mut Entry) -> Result<bool, Stop> {
        // A problem is left as it is.
        if e.v.problem.is_some() {
            return Ok(match op {
                Op::Diff => {
                    self.show_diffs(e)?;
                    true
                }
                Op::Details => {
                    self.display_details(&e.v);
                    true
                }
                Op::Discard => false,
                _ => true,
            });
        }
        let a = match op {
            Op::Diff => {
                self.show_diffs(e)?;
                return Ok(true);
            }
            Op::Details => {
                self.display_details(&e.v);
                return Ok(true);
            }
            Op::Discard => return Ok(false),
            Op::Set(a) => a,
            Op::FavorChanged => match e.v.favor_changed {
                Some(a) => a,
                None => return Ok(false),
            },
            Op::Invert => match e.v.action {
                Action::LtoR(_) => SetAction::Left,
                Action::RtoL(_) => SetAction::Right,
                _ => return Ok(false),
            },
            // A conflict keeps its reason.
            Op::Skip if matches!(e.v.action, Action::Skip(_)) => return Ok(true),
            Op::Skip => SetAction::Skip,
        };
        self.set_action(e, a)?;
        Ok(true)
    }

    fn set_action(&mut self, e: &mut Entry, a: SetAction) -> Result<(), Stop> {
        match self.call(Command::SetAction(e.i, a))? {
            Event::Item(_, v) => e.v = v,
            ev => return Err(Stop::Error(format!("unexpected {ev:?}"))),
        }
        Ok(())
    }

    /// `ignore_pref`: ignores the current item's path, extension or name
    /// from now on, and drops the items now ignored.
    fn ignore_pref(
        &mut self,
        prev: &mut Vec<Entry>,
        ril: &mut VecDeque<Entry>,
        kind: IgnoreKind,
        what: &str,
    ) -> Result<(), Stop> {
        let Some(ri) = ril.front().cloned() else {
            return Ok(());
        };
        self.display("  ");
        let items = match self.call(Command::Ignore(ri.i, kind))? {
            Event::Items { items, .. } => items,
            e => return Err(Stop::Error(format!("unexpected {e:?}"))),
        };
        self.display(&format!("  Permanently ignoring {what}\n"));
        self.display(&format!(
            "  To un-ignore, edit {} and restart {MY_NAME}\n",
            self.settings().profile_path.display()
        ));
        // `nukeIgnoredRis`, on `ri::prev` and on `ril`, which both start
        // with the item ignored.
        let kept: HashMap<String, (usize, ItemView)> = items
            .into_iter()
            .enumerate()
            .map(|(i, v)| (v.path_string.clone(), (i, v)))
            .collect();
        let nuke = |l: Vec<Entry>| -> Vec<Entry> {
            l.into_iter()
                .filter_map(|e| {
                    kept.get(&e.v.path_string).map(|(i, v)| Entry {
                        i: *i,
                        v: v.clone(),
                    })
                })
                .collect()
        };
        let mut p = std::mem::take(prev);
        p.push(ri);
        *prev = nuke(p);
        *ril = nuke(ril.drain(..).collect()).into();
        Ok(())
    }

    /// `showdiffs`: through `PAGER` if it is set.
    fn show_diffs(&mut self, e: &Entry) -> Result<(), Stop> {
        let (shown, errors) = match self.call(Command::Diff(e.i))? {
            Event::Diff { shown, errors } => (shown, errors),
            ev => return Err(Stop::Error(format!("unexpected {ev:?}"))),
        };
        for (title, text) in shown {
            let text = self.color_diff(&text);
            let s = format!("\n{title}\n\n{text}\n\n");
            match std::env::var_os("PAGER") {
                Some(pager) => {
                    self.restore_terminal();
                    page(&pager, &s);
                    self.setup_terminal();
                }
                None => infos::print(&s),
            }
        }
        for err in errors {
            infos::print(&format!("{err}\n"));
        }
        Ok(())
    }

    /// `colorDiff`.
    fn color_diff(&self, text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        for line in text.split_inclusive('\n') {
            let c = match line.as_bytes().first() {
                Some(b'+') => Some(Color::DiffAdd),
                Some(b'-') => Some(Color::DiffDel),
                Some(b'@') => Some(Color::DiffLoc),
                _ => None,
            };
            match c {
                Some(c) => {
                    out.push_str(self.color(c));
                    out.push_str(line);
                    out.push_str(self.color(Color::Reset));
                }
                None => out.push_str(line),
            }
        }
        out
    }
}

/// `ritest`: the item matches the condition, if any.
fn ritest(ripred: &[Pred], v: &ItemView) -> bool {
    ripred.last().is_none_or(|p| p.test(v))
}

/// `forward`: `n` items on (back, if negative), stopping at the ends.
fn forward(mut n: i64, prev: &mut Vec<Entry>, ril: &mut VecDeque<Entry>) {
    while n != 0 {
        if n < 0 {
            let Some(e) = prev.pop() else {
                return;
            };
            ril.push_front(e);
            n += 1;
        } else {
            let Some(e) = ril.pop_front() else {
                // Past the end: back to the last.
                ril.extend(prev.pop());
                return;
            };
            prev.push(e);
            n -= 1;
        }
    }
}

/// Shows `s` through the pager (`System.open_process_out`).
fn page(pager: &std::ffi::OsStr, s: &str) {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let mut c = if cfg!(windows) {
        let mut c = Command::new("cmd");
        c.arg("/c").arg(pager);
        c
    } else {
        let mut c = Command::new("/bin/sh");
        c.arg("-c").arg(pager);
        c
    };
    if let Ok(mut child) = c.stdin(Stdio::piped()).spawn() {
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(s.as_bytes());
        }
        let _ = child.wait();
    }
}
