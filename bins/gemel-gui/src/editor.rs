//! The profile editor's state, without a toolkit: the profile's
//! preferences (`editProfile`), one being edited (`editPreference`), the
//! tree a preference is added from (`addPreference`), and the
//! documentation shown below them (`documentPreference`).
//!
//! What a profile is read as, the text written back and the formatting of
//! the documentation are `gemel_ui_api::profile_editor`'s.

use std::collections::HashSet;

use gemel_ui_api::profile_editor::{
    Catalog, Node, NodeKind, PrefType, Setting, capitalize, format_doc,
};

/// The profile being edited: its preferences, in the order they will be
/// written (those read, by name, then those added), shown sorted by name.
#[derive(Debug)]
pub struct Profile {
    /// Its name.
    pub name: String,
    settings: Vec<Setting>,
    /// The order shown is by name descending, the Name column's header
    /// having been clicked.
    descending: bool,
    /// The setting selected, by its place in `settings`.
    selected: Option<usize>,
    modified: bool,
}

impl Profile {
    /// Profile `name`, with the settings read from it.
    pub fn new(name: &str, settings: Vec<Setting>) -> Profile {
        Profile {
            name: name.to_owned(),
            settings,
            descending: false,
            selected: None,
            modified: false,
        }
    }

    /// The settings, as they will be written.
    pub fn settings(&self) -> &[Setting] {
        &self.settings
    }

    /// The settings' places, in the order shown: by name, as GTK collates
    /// them (case aside, then as they are).
    fn order(&self) -> Vec<usize> {
        let mut order: Vec<usize> = (0..self.settings.len()).collect();
        let key = |i: &usize| {
            let n = &self.settings[*i].name;
            (n.to_lowercase(), n.clone())
        };
        order.sort_by_key(key);
        if self.descending {
            order.reverse();
        }
        order
    }

    /// The rows shown: each setting's name, type and values
    /// (`formatValue`).
    pub fn rows(&self) -> Vec<(String, &'static str, String)> {
        self.order()
            .into_iter()
            .map(|i| {
                let s = &self.settings[i];
                (s.name.clone(), s.typ.name(), s.values.join(", "))
            })
            .collect()
    }

    /// The row shown selected, if any.
    pub fn selected_row(&self) -> Option<usize> {
        let sel = self.selected?;
        self.order().iter().position(|&i| i == sel)
    }

    /// Selects row `row` (as shown).
    pub fn select_row(&mut self, row: usize) {
        self.selected = self.order().get(row).copied();
    }

    /// The setting selected, by its place.
    pub fn selected(&self) -> Option<usize> {
        self.selected
    }

    /// The setting at `i`.
    pub fn setting(&self, i: usize) -> &Setting {
        &self.settings[i]
    }

    /// Whether the rows are shown by name descending.
    pub fn descending(&self) -> bool {
        self.descending
    }

    /// The Name column's header: sorts the other way.
    pub fn toggle_order(&mut self) {
        self.descending = !self.descending;
    }

    /// Where the setting of preference `name` is.
    pub fn position(&self, name: &str) -> Option<usize> {
        self.settings.iter().position(|s| s.name == name)
    }

    /// Sets the values of the setting at `i`: whether they changed.
    pub fn set_values(&mut self, i: usize, values: Vec<String>) -> bool {
        if self.settings[i].values == values {
            return false;
        }
        self.settings[i].values = values;
        self.modified = true;
        true
    }

    /// Adds a setting, after the others.
    pub fn add(&mut self, setting: Setting) {
        self.settings.push(setting);
        self.modified = true;
    }

    /// Removes the setting at `i`; nothing is selected then.
    pub fn remove(&mut self, i: usize) {
        self.settings.remove(i);
        self.selected = None;
        self.modified = true;
    }

    /// Whether there is something to write (Apply).
    pub fn modified(&self) -> bool {
        self.modified
    }

    /// The settings have been written.
    pub fn saved(&mut self) {
        self.modified = false;
    }
}

/// What a preference being edited is edited as.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValueKind {
    /// True or False.
    Bool,
    /// A line of text.
    Text,
    /// A list of values, one edited at a time.
    List,
}

/// Where the values of a preference being edited go.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    /// To the setting at this place.
    Setting(usize),
    /// To a setting added to the profile.
    New,
}

/// A preference being edited (`editPreference`): its values as they are
/// edited, kept apart from the profile until OK.
#[derive(Clone, Debug)]
pub struct PrefEdit {
    /// The preference.
    pub name: String,
    /// Its type.
    pub typ: PrefType,
    /// Where its values go.
    pub target: Target,
    /// A line of text: what it is.
    pub text: String,
    /// True or False: which.
    pub truth: bool,
    /// A list: its values.
    pub items: Vec<String>,
    /// A list: the value selected.
    pub item: Option<usize>,
    /// A list: the value being edited ("Edited item").
    pub entry: String,
}

impl PrefEdit {
    /// Preference `name`, with `values` (at least one, unless it is a
    /// list).
    pub fn new(name: &str, typ: PrefType, values: Vec<String>, target: Target) -> PrefEdit {
        let first = values.first().cloned().unwrap_or_default();
        PrefEdit {
            name: name.to_owned(),
            typ,
            target,
            truth: first == "true" || first == "yes",
            text: first,
            items: if typ.is_list() { values } else { Vec::new() },
            item: None,
            entry: String::new(),
        }
    }

    /// What it is edited as.
    pub fn kind(&self) -> ValueKind {
        match self.typ {
            PrefType::Bool | PrefType::BoolDef => ValueKind::Bool,
            PrefType::Int | PrefType::String => ValueKind::Text,
            PrefType::StringList | PrefType::Custom | PrefType::Unknown => ValueKind::List,
        }
    }

    /// The values, as OK takes them.
    pub fn values(&self) -> Vec<String> {
        match self.kind() {
            ValueKind::Bool => vec![if self.truth { "true" } else { "false" }.to_owned()],
            ValueKind::Text => vec![self.text.clone()],
            ValueKind::List => self.items.clone(),
        }
    }

    /// Selects value `i`, which is then the one edited.
    pub fn select_item(&mut self, i: usize) {
        if let Some(v) = self.items.get(i) {
            self.item = Some(i);
            self.entry = v.clone();
        }
    }

    /// Add (or Enter in the entry): the value edited, after the others,
    /// selected.
    pub fn add_item(&mut self) {
        self.items.push(self.entry.clone());
        self.select_item(self.items.len() - 1);
    }

    /// Remove: the value selected; the one after is selected, or the one
    /// before if it was the last.
    pub fn remove_item(&mut self) {
        let Some(i) = self.item else {
            return;
        };
        self.items.remove(i);
        self.item = None;
        if i < self.items.len() {
            self.select_item(i);
        } else if i > 0 {
            self.select_item(i - 1);
        }
    }

    /// Whether Edit would change the value selected.
    pub fn can_edit(&self) -> bool {
        self.item.is_some_and(|i| self.items[i] != self.entry)
    }

    /// Edit: the value selected is the one edited.
    pub fn edit_item(&mut self) {
        if let Some(i) = self.item {
            self.items[i] = self.entry.clone();
        }
    }

    /// Whether the value selected can go up.
    pub fn can_go_up(&self) -> bool {
        self.item.is_some_and(|i| i > 0)
    }

    /// Whether it can go down.
    pub fn can_go_down(&self) -> bool {
        self.item.is_some_and(|i| i + 1 < self.items.len())
    }

    /// Up: the value selected goes before the one before it.
    pub fn go_up(&mut self) {
        if let Some(i) = self.item.filter(|_| self.can_go_up()) {
            self.items.swap(i, i - 1);
            self.item = Some(i - 1);
        }
    }

    /// Down: after the one after it.
    pub fn go_down(&mut self) {
        if let Some(i) = self.item.filter(|_| self.can_go_down()) {
            self.items.swap(i, i + 1);
            self.item = Some(i + 1);
        }
    }
}

/// A row of the tree shown.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeRow {
    /// What it shows.
    pub title: String,
    /// What it is.
    pub kind: NodeKind,
    /// How deep it is.
    pub depth: usize,
    /// Whether it has rows under it.
    pub parent: bool,
    /// Whether they are shown.
    pub expanded: bool,
}

/// `addPreference`'s tree: the rows expanded, and the one selected.
#[derive(Debug)]
pub struct Tree {
    nodes: Vec<Node>,
    /// The rows expanded, by where they are.
    expanded: HashSet<Vec<usize>>,
    selected: Option<Vec<usize>>,
    /// Expand all has been pressed (and Collapse all not since).
    all: bool,
}

impl Tree {
    /// The tree, its first row expanded.
    pub fn new(nodes: Vec<Node>) -> Tree {
        let expanded = if nodes.is_empty() {
            HashSet::new()
        } else {
            HashSet::from([vec![0]])
        };
        Tree {
            nodes,
            expanded,
            selected: None,
            all: false,
        }
    }

    fn node(&self, at: &[usize]) -> &Node {
        let mut n = &self.nodes[at[0]];
        for &i in &at[1..] {
            n = &n.children[i];
        }
        n
    }

    /// The rows shown, each with where it is.
    fn shown(&self) -> Vec<(Vec<usize>, &Node)> {
        fn walk<'a>(
            t: &'a Tree,
            nodes: &'a [Node],
            at: &mut Vec<usize>,
            out: &mut Vec<(Vec<usize>, &'a Node)>,
        ) {
            for (i, n) in nodes.iter().enumerate() {
                at.push(i);
                out.push((at.clone(), n));
                if t.expanded.contains(at) {
                    walk(t, &n.children, at, out);
                }
                at.pop();
            }
        }
        let mut out = Vec::new();
        walk(self, &self.nodes, &mut Vec::new(), &mut out);
        out
    }

    /// The rows shown.
    pub fn rows(&self) -> Vec<TreeRow> {
        self.shown()
            .into_iter()
            .map(|(at, n)| TreeRow {
                title: n.title.clone(),
                kind: n.kind,
                depth: at.len() - 1,
                parent: !n.children.is_empty(),
                expanded: self.expanded.contains(&at),
            })
            .collect()
    }

    /// The row selected, as shown.
    pub fn selected_row(&self) -> Option<usize> {
        let sel = self.selected.as_ref()?;
        self.shown().iter().position(|(at, _)| at == sel)
    }

    /// A click on row `row`: selects it, and expands it or collapses it.
    pub fn click(&mut self, row: usize) {
        let Some((at, n)) = self.shown().into_iter().nth(row) else {
            return;
        };
        if !n.children.is_empty() && !self.expanded.remove(&at) {
            self.expanded.insert(at.clone());
        }
        self.selected = Some(at);
    }

    /// Selects row `row`, as a screen reader does.
    pub fn select(&mut self, row: usize) {
        if let Some((at, _)) = self.shown().into_iter().nth(row) {
            self.selected = Some(at);
        }
    }

    /// Whether Expand all has been pressed (the button then says Collapse
    /// all).
    pub fn all_expanded(&self) -> bool {
        self.all
    }

    /// Expand all, or Collapse all.
    pub fn toggle_all(&mut self) {
        self.all = !self.all;
        self.expanded.clear();
        if self.all {
            fn walk(nodes: &[Node], at: &mut Vec<usize>, out: &mut HashSet<Vec<usize>>) {
                for (i, n) in nodes.iter().enumerate() {
                    at.push(i);
                    if !n.children.is_empty() {
                        out.insert(at.clone());
                        walk(&n.children, at, out);
                    }
                    at.pop();
                }
            }
            walk(&self.nodes, &mut Vec::new(), &mut self.expanded);
        }
        // A row selected under one collapsed isn't shown: none is.
        if self.selected_row().is_none() {
            self.selected = None;
        }
    }

    /// The preference selected, if a preference is (`getSelectedPref`).
    pub fn selected_pref(&self) -> Option<String> {
        let n = self.node(self.selected.as_ref()?);
        (n.kind == NodeKind::Pref).then(|| n.title.clone())
    }
}

/// `documentPreference`: preference `name`'s one-line documentation,
/// capitalized, and its full one as markdown for Slint's `StyledText`.
pub fn documentation(catalog: &Catalog, name: &str) -> (String, String) {
    let (short, long) = catalog.documentation(name);
    (capitalize(&short), markdown(&format_doc(&long)))
}

/// `insertMarkup`'s reading of GTK markup, as markdown: `<i>` in italics,
/// `<tt>` as code, other tags dropped, `&amp;`, `&lt;`, `&gt;`, `&quot;`
/// and `&apos;` decoded and other entities left out. Each line is a line
/// of its own, an empty one a no-break space. Unlike `uigtk3`, `&#39;`
/// (which `Glib.Markup.escape_text` makes of an apostrophe) is an
/// apostrophe.
pub fn markdown(markup: &str) -> String {
    lines(markup)
        .iter()
        .map(|spans| {
            if spans.iter().all(|(_, t)| t.trim().is_empty()) {
                return "\u{a0}".to_owned();
            }
            let mut out = String::new();
            for (style, text) in trim_spans(spans) {
                match style {
                    Style::Plain => out += &escape(&text),
                    Style::Italic => {
                        let body = text.trim();
                        let (lead, trail) = (
                            &text[..text.len() - text.trim_start().len()],
                            &text[text.trim_end().len()..],
                        );
                        out += &escape(lead);
                        if !body.is_empty() {
                            out += &format!("*{}*", escape(body));
                        }
                        out += &escape(trail);
                    }
                    Style::Code => out += &code_span(&text),
                }
            }
            out
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The text `markdown` shows of `markup`: what Slint's `StyledText` should
/// have once parsed, line by line.
pub fn shown_text(markup: &str) -> String {
    lines(markup)
        .iter()
        .map(|spans| {
            let line: String = trim_spans(spans).into_iter().map(|(_, t)| t).collect();
            if line.is_empty() {
                "\u{a0}".to_owned()
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Style {
    Plain,
    Italic,
    Code,
}

/// The lines of `markup`, each the text of each style in it.
fn lines(markup: &str) -> Vec<Vec<(Style, String)>> {
    let mut stack: Vec<Option<Style>> = Vec::new();
    let mut lines = vec![Vec::new()];
    let style = |stack: &[Option<Style>]| {
        if stack.contains(&Some(Style::Code)) {
            Style::Code
        } else if stack.contains(&Some(Style::Italic)) {
            Style::Italic
        } else {
            Style::Plain
        }
    };
    let push = |lines: &mut Vec<Vec<(Style, String)>>, st: Style, text: &str| {
        for (k, part) in text.split('\n').enumerate() {
            if k > 0 {
                lines.push(Vec::new());
            }
            let line = lines.last_mut().unwrap();
            match line.last_mut() {
                // One span for text of one style, tags nested or not.
                Some((last, text)) if *last == st => *text += part,
                _ if !part.is_empty() => line.push((st, part.to_owned())),
                _ => {}
            }
        }
    };
    let mut rest = markup;
    while !rest.is_empty() {
        if let Some((tag, len)) = tag_at(rest) {
            match tag {
                Tag::Open(name) => stack.push(match name {
                    "i" => Some(Style::Italic),
                    "tt" => Some(Style::Code),
                    _ => None,
                }),
                Tag::Close => {
                    stack.pop();
                }
                Tag::Entity(e) => {
                    let text = match e {
                        "amp" => "&",
                        "lt" => "<",
                        "gt" => ">",
                        "quot" => "\"",
                        "apos" | "#39" => "'",
                        _ => "",
                    };
                    push(&mut lines, style(&stack), text);
                }
            }
            rest = &rest[len..];
            continue;
        }
        let next = rest[1..].find(['<', '&']).map_or(rest.len(), |i| i + 1);
        push(&mut lines, style(&stack), &rest[..next]);
        rest = &rest[next..];
    }
    lines
}

enum Tag<'a> {
    Open(&'a str),
    Close,
    Entity(&'a str),
}

/// `markupRe` at the start of `s` (with `&#39;` too): the tag or entity,
/// and its length.
fn tag_at(s: &str) -> Option<(Tag<'_>, usize)> {
    let letters = |t: &str| t.bytes().take_while(u8::is_ascii_lowercase).count();
    if let Some(r) = s.strip_prefix("</") {
        let n = letters(r);
        return (n > 0 && r[n..].starts_with('>')).then_some((Tag::Close, n + 3));
    }
    if let Some(r) = s.strip_prefix('<') {
        let n = letters(r);
        return (n > 0 && r[n..].starts_with('>')).then(|| (Tag::Open(&r[..n]), n + 2));
    }
    let r = s.strip_prefix('&')?;
    if r.starts_with("#39;") {
        return Some((Tag::Entity("#39"), 5));
    }
    let n = letters(r);
    (n > 0 && r[n..].starts_with(';')).then(|| (Tag::Entity(&r[..n]), n + 2))
}

/// A line's spans without the blanks it starts and ends with, which
/// markdown would drop.
fn trim_spans(spans: &[(Style, String)]) -> Vec<(Style, String)> {
    let mut spans: Vec<(Style, String)> = spans.to_vec();
    while let Some((_, t)) = spans.first_mut() {
        let trimmed = t.trim_start();
        if trimmed.is_empty() {
            spans.remove(0);
        } else {
            *t = trimmed.to_owned();
            break;
        }
    }
    while let Some((_, t)) = spans.last_mut() {
        let trimmed = t.trim_end();
        if trimmed.is_empty() {
            spans.pop();
        } else {
            *t = trimmed.to_owned();
            break;
        }
    }
    spans
}

/// Text that markdown reads as it is: each ASCII punctuation mark escaped.
fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_ascii_punctuation() {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// A code span showing `s` as it is.
fn code_span(s: &str) -> String {
    let longest = s.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    let fence = "`".repeat(longest + 1);
    // One space each side is taken off when there is one each side.
    let pad = s.starts_with(['`', ' ']) || s.ends_with(['`', ' ']);
    let pad = if pad && !s.trim().is_empty() { " " } else { "" };
    format!("{fence}{pad}{s}{pad}{fence}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setting(name: &str, typ: PrefType, values: &[&str]) -> Setting {
        Setting {
            name: name.to_owned(),
            typ,
            values: values.iter().map(|v| v.to_string()).collect(),
        }
    }

    #[test]
    fn the_profile_is_shown_by_name_and_written_as_it_was_read() {
        let mut p = Profile::new(
            "p",
            vec![
                setting("batch", PrefType::Bool, &["true"]),
                setting("root", PrefType::StringList, &["/a", "/b"]),
            ],
        );
        p.add(setting("Label", PrefType::String, &["l"]));
        let names = |p: &Profile| p.rows().into_iter().map(|r| r.0).collect::<Vec<_>>();
        assert_eq!(names(&p), ["batch", "Label", "root"]);
        assert_eq!(p.rows()[2].1, "text list");
        assert_eq!(p.rows()[2].2, "/a, /b");
        p.select_row(1);
        assert_eq!(p.selected(), Some(2));
        p.toggle_order();
        assert_eq!(names(&p), ["root", "Label", "batch"]);
        assert_eq!(p.selected_row(), Some(1));
        // Written in the order read, then added.
        let written: Vec<&str> = p.settings().iter().map(|s| s.name.as_str()).collect();
        assert_eq!(written, ["batch", "root", "Label"]);
        assert!(p.modified());
        p.saved();
        assert!(!p.set_values(0, vec!["true".to_owned()]) && !p.modified());
        assert!(p.set_values(0, vec!["false".to_owned()]) && p.modified());
        p.remove(2);
        assert_eq!(p.selected(), None);
        assert_eq!(p.position("root"), Some(1));
    }

    #[test]
    fn a_preference_is_edited_as_editpreference_edits_it() {
        let b = PrefEdit::new("batch", PrefType::Bool, vec!["yes".into()], Target::New);
        assert_eq!(b.kind(), ValueKind::Bool);
        // `yes` is true, and comes back so.
        assert_eq!(b.values(), ["true"]);
        let t = PrefEdit::new("height", PrefType::Int, vec!["0".into()], Target::New);
        assert_eq!(
            (t.kind(), t.values()),
            (ValueKind::Text, vec!["0".to_owned()])
        );

        let mut l = PrefEdit::new(
            "root",
            PrefType::StringList,
            vec!["a".into(), "b".into(), "c".into()],
            Target::Setting(0),
        );
        assert_eq!(l.kind(), ValueKind::List);
        assert!(!l.can_edit() && !l.can_go_up() && !l.can_go_down());
        l.select_item(1);
        assert_eq!(l.entry, "b");
        assert!(!l.can_edit() && l.can_go_up() && l.can_go_down());
        l.entry = "B".into();
        assert!(l.can_edit());
        l.edit_item();
        assert_eq!(l.items, ["a", "B", "c"]);
        l.go_down();
        assert_eq!(
            (l.items.clone(), l.item),
            (vec!["a".into(), "c".into(), "B".into()], Some(2))
        );
        assert!(!l.can_go_down());
        // The last one removed: the one before is selected.
        l.remove_item();
        assert_eq!(
            (l.items.clone(), l.item, l.entry.clone()),
            (vec!["a".into(), "c".into()], Some(1), "c".into())
        );
        l.select_item(0);
        l.remove_item();
        assert_eq!((l.items.clone(), l.item), (vec!["c".to_owned()], Some(0)));
        l.entry = "d".into();
        l.add_item();
        assert_eq!(
            (l.values(), l.item),
            (vec!["c".into(), "d".into()], Some(1))
        );
        l.remove_item();
        l.remove_item();
        assert_eq!((l.values(), l.item), (Vec::<String>::new(), None));
    }

    #[test]
    fn the_tree_expands_as_addpreferences_does() {
        let leaf = |t: &str| Node {
            title: t.to_owned(),
            kind: NodeKind::Pref,
            children: Vec::new(),
        };
        let topic = |t: &str, c: Vec<Node>| Node {
            title: t.to_owned(),
            kind: NodeKind::Topic,
            children: c,
        };
        let group = |t: &str, c: Vec<Node>| Node {
            title: t.to_owned(),
            kind: NodeKind::Group,
            children: c,
        };
        let mut t = Tree::new(vec![
            group(
                "G1",
                vec![topic("T1", vec![leaf("a"), leaf("b")]), leaf("key")],
            ),
            group("G2", vec![leaf("c")]),
        ]);
        let titles = |t: &Tree| t.rows().into_iter().map(|r| r.title).collect::<Vec<_>>();
        // The first group expanded.
        assert_eq!(titles(&t), ["G1", "T1", "key", "G2"]);
        assert_eq!(t.rows()[1].depth, 1);
        assert!(t.rows()[0].expanded && t.rows()[1].parent && !t.rows()[1].expanded);
        t.click(1);
        assert_eq!(titles(&t), ["G1", "T1", "a", "b", "key", "G2"]);
        assert_eq!(t.selected_pref(), None);
        t.click(3);
        assert_eq!(t.selected_pref().as_deref(), Some("b"));
        assert_eq!(t.selected_row(), Some(3));
        t.click(0);
        assert_eq!(titles(&t), ["G1", "G2"]);
        assert_eq!(t.selected_row(), Some(0));
        t.toggle_all();
        assert!(t.all_expanded());
        assert_eq!(titles(&t), ["G1", "T1", "a", "b", "key", "G2", "c"]);
        t.select(6);
        assert_eq!(t.selected_pref().as_deref(), Some("c"));
        t.toggle_all();
        assert_eq!(titles(&t), ["G1", "G2"]);
        assert_eq!(t.selected_pref(), None);
    }

    #[test]
    fn markup_is_markdown() {
        assert_eq!(
            markdown(
                "Use <tt>-x</tt> or <i>this one</i> &amp; <b>that</b> &#39;q&#39;.\nNext *line*"
            ),
            "Use `-x` or *this one* \\& that \\'q\\'\\.\nNext \\*line\\*"
        );
        assert_eq!(markdown("a <i> b </i>c"), "a  *b* c");
        assert_eq!(markdown("<tt>`x`</tt> <tt> y </tt>"), "`` `x` `` `  y `");
        assert_eq!(markdown("one\n\n  two"), "one\n\u{a0}\ntwo");
        assert_eq!(
            shown_text("one\n\n  <i>two</i> &lt;3"),
            "one\n\u{a0}\ntwo <3"
        );
    }

    #[test]
    fn every_preferences_documentation_reads_as_markdown() {
        let c = Catalog::new().unwrap();
        let mut names = Vec::new();
        fn walk(n: &Node, out: &mut Vec<String>) {
            if n.kind == NodeKind::Pref {
                out.push(n.title.clone());
            }
            for c in &n.children {
                walk(c, out);
            }
        }
        for n in c.tree() {
            walk(&n, &mut names);
        }
        assert!(names.len() > 90, "{}", names.len());
        for name in names {
            let (_, long) = c.documentation(&name);
            let markup = format_doc(&long);
            let md = markdown(&markup);
            let styled = slint::StyledText::from_markdown(&md)
                .unwrap_or_else(|e| panic!("{name}: {e}\n{md}"));
            let raw = i_slint_core::styled_text::get_raw_text(&styled);
            assert_eq!(raw, shown_text(&markup), "{name}:\n{md}");
        }
    }
}
