//! What `uigtk3`'s profile editor needs, without a toolkit: the
//! preferences typed as `Prefs` types them, with their documentation and
//! the groups and topics they are listed under; a profile's settings, as
//! the editor reads them; the text it writes back; and the documentation,
//! as `documentPreference` formats it.
//!
//! Ports `uigtk3.ml`'s `nameOfType`, `defaultValue`, `editProfile`'s
//! reading (`group`, `normalizeValue`) and writing (`applyCommand`),
//! `addPreference`'s tree, and `documentPreference`'s `formatDoc`. The
//! session reads and writes profiles ([`crate::session::Command`]).

use gemel_base::prefs::{Category, Kind, Prefs, Topic};
use gemel_base::util::Error;

/// A preference's type, as the editor tells them apart (`Prefs.typ`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrefType {
    /// `BOOL`.
    Bool,
    /// `BOOLDEF`: true, false or `default`.
    BoolDef,
    /// `INT`.
    Int,
    /// `STRING`.
    String,
    /// `STRING_LIST`: each setting adds to the list.
    StringList,
    /// `CUSTOM`: a parser of its own.
    Custom,
    /// `UNKNOWN`: no such preference.
    Unknown,
}

impl PrefType {
    fn of(kind: Option<Kind>) -> PrefType {
        match kind {
            Some(Kind::Bool) => PrefType::Bool,
            Some(Kind::BoolDef) => PrefType::BoolDef,
            Some(Kind::Int) => PrefType::Int,
            Some(Kind::String) => PrefType::String,
            Some(Kind::StringList) => PrefType::StringList,
            Some(Kind::Custom) => PrefType::Custom,
            None => PrefType::Unknown,
        }
    }

    /// `nameOfType`.
    pub fn name(self) -> &'static str {
        match self {
            PrefType::Bool | PrefType::BoolDef => "boolean",
            PrefType::Int => "integer",
            PrefType::String => "text",
            PrefType::StringList => "text list",
            PrefType::Custom => "custom",
            PrefType::Unknown => "unknown",
        }
    }

    /// `defaultValue`: what a preference added to a profile starts with.
    pub fn default_value(self) -> Vec<String> {
        match self {
            PrefType::Bool | PrefType::BoolDef => vec!["true".to_owned()],
            PrefType::Int => vec!["0".to_owned()],
            PrefType::String => vec![String::new()],
            PrefType::StringList | PrefType::Custom | PrefType::Unknown => Vec::new(),
        }
    }

    /// `editPreference`'s `isList`: edited as a list of values.
    pub fn is_list(self) -> bool {
        matches!(
            self,
            PrefType::StringList | PrefType::Custom | PrefType::Unknown
        )
    }

    /// `normalizeValue`: what the editor keeps of a preference's values in
    /// a profile, the last for a single value; none for a three-state
    /// boolean left to its default.
    fn normalize(self, mut values: Vec<String>) -> Vec<String> {
        match self {
            PrefType::Bool | PrefType::Int | PrefType::String => last_one(values),
            PrefType::StringList | PrefType::Custom | PrefType::Unknown => values,
            PrefType::BoolDef => {
                values = last_one(values);
                if values == ["default"] || values == ["auto"] {
                    Vec::new()
                } else {
                    values
                }
            }
        }
    }
}

fn last_one(mut values: Vec<String>) -> Vec<String> {
    values.drain(..values.len().saturating_sub(1));
    values
}

/// A preference a profile sets, with its values in the order the profile
/// gives them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Setting {
    /// Its name, canonical.
    pub name: String,
    /// Its type.
    pub typ: PrefType,
    /// Its values.
    pub values: Vec<String>,
}

/// `editProfile`'s reading of profile `name`: its lines (includes
/// expanded, comments left out), by canonical name in byte order, each
/// preference's values together in the order they came, as
/// `normalizeValue` keeps them; preferences left with no value are left
/// out.
///
/// As `Prefs.readAFile`, it records the files read, so that a profile
/// loaded since is taken to have changed if they change.
pub fn read_settings(prefs: &mut Prefs, name: &str) -> Result<Vec<Setting>, Error> {
    let lines = prefs.read_a_file(name, true, true)?;
    let mut named: Vec<(String, String)> = lines
        .into_iter()
        .map(|l| (prefs.canonical_name(&l.name).to_owned(), l.value))
        .collect();
    // `List.stable_sort`.
    named.sort_by(|(a, _), (b, _)| a.cmp(b));
    let mut grouped: Vec<(String, Vec<String>)> = Vec::new();
    for (name, value) in named {
        match grouped.last_mut() {
            Some((n, values)) if *n == name => values.push(value),
            _ => grouped.push((name, vec![value])),
        }
    }
    Ok(grouped
        .into_iter()
        .filter_map(|(name, values)| {
            let typ = PrefType::of(prefs.kind(&name));
            let values = typ.normalize(values);
            (!values.is_empty()).then_some(Setting { name, typ, values })
        })
        .collect())
}

/// `applyCommand`'s text: the header, then a line for each value of each
/// setting, in order, as they are.
pub fn profile_text(settings: &[Setting]) -> String {
    let mut text = String::from("# Unison preferences\n");
    for s in settings {
        for v in &s.values {
            text += &format!("{} = {v}\n", s.name);
        }
    }
    text
}

/// A row of `addPreference`'s tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Node {
    /// What it shows.
    pub title: String,
    /// What it is.
    pub kind: NodeKind,
    /// The rows under it.
    pub children: Vec<Node>,
}

/// What a row of [`Node`]s is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NodeKind {
    /// A group (Basic, Advanced, Expert), shown bold.
    Group,
    /// A topic within a group, shown italic.
    Topic,
    /// A preference, which can be added.
    Pref,
}

impl Node {
    fn new(title: &str, kind: NodeKind) -> Node {
        Node {
            title: title.to_owned(),
            kind,
            children: Vec::new(),
        }
    }
}

/// The preferences, with their types and documentation, as the session's
/// own registers them.
pub struct Catalog {
    prefs: Prefs,
}

impl Catalog {
    /// Every preference, registered.
    pub fn new() -> Result<Catalog, Error> {
        Ok(Catalog {
            prefs: crate::new_prefs()?,
        })
    }

    /// `Prefs.typ`.
    pub fn typ(&self, name: &str) -> PrefType {
        PrefType::of(self.prefs.kind(name))
    }

    /// `Prefs.documentation`: the one-line documentation and the full one,
    /// both empty for a preference that isn't documented.
    pub fn documentation(&self, name: &str) -> (String, String) {
        self.prefs.documentation(name)
    }

    /// `addPreference`'s tree of the documented preferences (`Prefs.list
    /// false`): the basic ones, by topic (General first), with `label` and
    /// `key` in the group itself; the advanced ones, by topic (General
    /// last); and the expert ones. Topics and groups left empty are left
    /// out.
    pub fn tree(&self) -> Vec<Node> {
        const TOPICS: [Topic; 7] = [
            Topic::Sync,
            Topic::Syncprocess,
            Topic::Syncprocess_CLI,
            Topic::CLI,
            Topic::GUI,
            Topic::Remote,
            Topic::Archive,
        ];
        const SHOWN: [&str; 2] = ["label", "key"];
        let topics = |first: Option<Topic>, last: Option<Topic>| -> Vec<Node> {
            first
                .into_iter()
                .chain(TOPICS)
                .chain(last)
                .map(|t| Node::new(t.title(), NodeKind::Topic))
                .collect()
        };
        let basic_topics: Vec<Topic> = std::iter::once(Topic::General).chain(TOPICS).collect();
        let advanced_topics: Vec<Topic> = TOPICS.into_iter().chain([Topic::General]).collect();
        let mut basic = Node::new("1 — Basic preferences", NodeKind::Group);
        basic.children = topics(Some(Topic::General), None);
        let mut advanced = Node::new("2 — Advanced preferences", NodeKind::Group);
        advanced.children = topics(None, Some(Topic::General));
        let mut expert = Node::new("3 — Expert preferences", NodeKind::Group);
        let mut top = Vec::new();
        for name in self.prefs.list(false) {
            let row = Node::new(&name, NodeKind::Pref);
            match self.prefs.category(&name) {
                None => top.push(row),
                Some(_) if SHOWN.contains(&name.as_str()) => basic.children.push(row),
                Some(Category::Basic(t)) => match basic_topics.iter().position(|&b| b == t) {
                    Some(i) => basic.children[i].children.push(row),
                    None => top.push(row),
                },
                Some(Category::Advanced(t)) => match advanced_topics.iter().position(|&a| a == t) {
                    Some(i) => advanced.children[i].children.push(row),
                    None => top.push(row),
                },
                Some(Category::Expert) => expert.children.push(row),
                Some(Category::Internal(_)) => top.push(row),
            }
        }
        // `purgeParents`.
        for g in [&mut basic, &mut advanced] {
            g.children
                .retain(|n| n.kind != NodeKind::Topic || !n.children.is_empty());
        }
        [basic, advanced, expert]
            .into_iter()
            .filter(|g| !g.children.is_empty())
            .chain(top)
            .collect()
    }
}

/// `String.capitalize_ascii`, as `documentPreference` shows the one-line
/// documentation.
pub fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_ascii_uppercase().to_string() + c.as_str(),
        None => String::new(),
    }
}

/// `documentPreference`'s `formatDoc`: a preference's full documentation,
/// written in LaTeX, as GTK markup: its lines joined, the text escaped,
/// `{\em ...}`, `\emph{...}` in `<i>`, `{\tt ...}`, `\verb|...|`,
/// `\ARG{...}` and `\texttt{...}` in `<tt>`, section references named,
/// `---` an em dash, `\par` a new line, and `\_` and `\$` underscores
/// (sic).
pub fn format_doc(doc: &str) -> String {
    let s = substitute(doc, |t| {
        let rest = t.strip_prefix('\n')?;
        Some((1 + spaces(rest), " ".to_owned()))
    });
    let s = escape_markup(&s);
    let s = substitute(&s, |t| {
        let rest = t.strip_prefix("{\\")?;
        let tag_len = rest.bytes().take_while(u8::is_ascii_lowercase).count();
        if tag_len == 0 {
            return None;
        }
        let (tag, rest) = rest.split_at(tag_len);
        let rest = rest.strip_prefix(' ')?;
        let (body, len) = braced_body(rest)?;
        let whole = 2 + tag_len + 1 + len;
        let tag = match tag {
            "em" => "i",
            "tt" => "tt",
            _ => return Some((whole, t[..whole].to_owned())),
        };
        Some((whole, format!("<{tag}>{body}</{tag}>")))
    });
    let s = substitute(&s, |t| {
        let rest = t.strip_prefix("\\verb|")?;
        let end = rest.find('|')?;
        Some((6 + end + 1, format!("<tt>{}</tt>", &rest[..end])))
    });
    let s = command(&s, "\\ARG{", "tt");
    let s = command(&s, "\\texttt{", "tt");
    let s = command(&s, "\\emph{", "i");
    let s = substitute(&s, |t| {
        let rest = t.strip_prefix("\\sectionref{")?;
        let (_, a) = braced_body(rest)?;
        let rest2 = rest[a..].strip_prefix('{')?;
        let (name, b) = braced_body(rest2)?;
        Some((12 + a + 1 + b, format!("Section '{name}'")))
    });
    let s = s.replace("---", "\u{2014}");
    let s = substitute(&s, |t| {
        let rest = t.strip_prefix("\\par")?;
        Some((4 + spaces(rest), "\n".to_owned()))
    });
    let s = substitute(&s, |t| {
        let rest = t.strip_prefix("\\_")?;
        Some((2 + spaces(rest), "_".to_owned()))
    });
    substitute(&s, |t| {
        let rest = t.strip_prefix("\\$")?;
        Some((2 + spaces(rest), "_".to_owned()))
    })
}

/// `Glib.Markup.escape_text`.
fn escape_markup(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out += "&amp;",
            '<' => out += "&lt;",
            '>' => out += "&gt;",
            '\'' => out += "&#39;",
            '"' => out += "&quot;",
            c => out.push(c),
        }
    }
    out
}

/// The number of spaces `s` starts with (` *`).
fn spaces(s: &str) -> usize {
    s.bytes().take_while(|&b| b == b' ').count()
}

/// `[^{}]*}`: the text up to a closing brace with no brace before it, and
/// the length with the brace.
fn braced_body(s: &str) -> Option<(&str, usize)> {
    let end = s.find(['{', '}'])?;
    (s.as_bytes()[end] == b'}').then(|| (&s[..end], end + 1))
}

/// `\name{...}` in `<tag>`.
fn command(s: &str, open: &str, tag: &str) -> String {
    substitute(s, |t| {
        let rest = t.strip_prefix(open)?;
        let (body, len) = braced_body(rest)?;
        Some((open.len() + len, format!("<{tag}>{body}</{tag}>")))
    })
}

/// `Str.global_substitute`, for a pattern that `matches` at the start of
/// the text it is given: its length, and what replaces it. Matches are
/// looked for from the left, each after the one before.
fn substitute(s: &str, matches: impl Fn(&str) -> Option<(usize, String)>) -> String {
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        match matches(&s[i..]) {
            Some((len, rep)) if len > 0 => {
                out += &rep;
                i += len;
            }
            _ => {
                let c = s[i..].chars().next().unwrap();
                out.push(c);
                i += c.len_utf8();
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn types_as_uigtk3_names_them() {
        let c = Catalog::new().unwrap();
        let t = |n| c.typ(n);
        assert_eq!(t("batch"), PrefType::Bool);
        assert_eq!(t("fastcheck"), PrefType::BoolDef);
        assert_eq!(t("height"), PrefType::Int);
        assert_eq!(t("label"), PrefType::String);
        assert_eq!(t("root"), PrefType::StringList);
        assert_eq!(t("path"), PrefType::Custom);
        assert_eq!(t("nosuchpref"), PrefType::Unknown);
        assert_eq!(PrefType::BoolDef.name(), "boolean");
        assert_eq!(PrefType::StringList.name(), "text list");
        assert_eq!(PrefType::Int.default_value(), ["0"]);
        assert_eq!(PrefType::String.default_value(), [""]);
        assert!(PrefType::Unknown.is_list() && !PrefType::BoolDef.is_list());
    }

    #[test]
    fn values_are_normalized_as_uigtk3_does() {
        let n = |t: PrefType, v: &[&str]| t.normalize(strings(v));
        assert_eq!(n(PrefType::Bool, &["true", "false"]), ["false"]);
        assert_eq!(n(PrefType::Int, &[]), Vec::<String>::new());
        assert_eq!(n(PrefType::StringList, &["a", "b"]), ["a", "b"]);
        assert_eq!(n(PrefType::Unknown, &["a", "b"]), ["a", "b"]);
        assert_eq!(
            n(PrefType::BoolDef, &["true", "default"]),
            Vec::<String>::new()
        );
        assert_eq!(n(PrefType::BoolDef, &["auto"]), Vec::<String>::new());
        assert_eq!(n(PrefType::BoolDef, &["auto", "yes"]), ["yes"]);
    }

    #[test]
    fn the_text_is_uigtk3s() {
        let s = |name: &str, typ, v: &[&str]| Setting {
            name: name.to_owned(),
            typ,
            values: strings(v),
        };
        assert_eq!(
            profile_text(&[
                s("batch", PrefType::Bool, &["true"]),
                s("path", PrefType::StringList, &["a b", "c"]),
            ]),
            "# Unison preferences\nbatch = true\npath = a b\npath = c\n"
        );
        assert_eq!(profile_text(&[]), "# Unison preferences\n");
    }

    #[test]
    fn the_tree_is_addpreferences() {
        let c = Catalog::new().unwrap();
        let tree = c.tree();
        let titles: Vec<&str> = tree.iter().map(|n| n.title.as_str()).collect();
        assert_eq!(
            titles,
            [
                "1 — Basic preferences",
                "2 — Advanced preferences",
                "3 — Expert preferences"
            ]
        );
        let basic = &tree[0];
        // The topics first, then `key` and `label`. The basic general
        // preferences are all for the command line, so that topic goes.
        assert_eq!(basic.children[0].title, "What to sync");
        assert_eq!(basic.children[0].kind, NodeKind::Topic);
        assert!(!basic.children.iter().any(|n| n.title == "General"));
        let prefs: Vec<&str> = basic
            .children
            .iter()
            .filter(|n| n.kind == NodeKind::Pref)
            .map(|n| n.title.as_str())
            .collect();
        assert_eq!(prefs, ["key", "label"]);
        assert_eq!(tree[1].children.last().unwrap().title, "General");
        // No topic is left empty, and every documented preference is
        // somewhere, once.
        let mut all = Vec::new();
        fn walk<'a>(n: &'a Node, all: &mut Vec<&'a str>) {
            if n.kind == NodeKind::Pref {
                all.push(n.title.as_str());
            }
            assert!(
                n.kind == NodeKind::Pref || !n.children.is_empty(),
                "{}",
                n.title
            );
            for c in &n.children {
                walk(c, all);
            }
        }
        for n in &tree {
            walk(n, &mut all);
        }
        all.sort();
        assert_eq!(all, c.prefs.list(false));
        assert!(!all.contains(&"doc"), "a command-line option");
    }

    #[test]
    fn docs_are_formatted_as_uigtk3_formats_them() {
        assert_eq!(
            format_doc(
                "When {\\tt x} is set,\n   \\texttt{a<b} \\verb|c d| and \\ARG{n}\
                 ---{\\em really}, \\emph{see} \\sectionref{s}{The Section}.\\par   Next \\_ \\$1 \
                 {\\bf kept} 'q' \"r\" & s"
            ),
            "When <tt>x</tt> is set, <tt>a&lt;b</tt> <tt>c d</tt> and <tt>n</tt>\u{2014}<i>really</i>, \
             <i>see</i> Section 'The Section'.\nNext __1 {\\bf kept} &#39;q&#39; &quot;r&quot; &amp; s"
        );
        // Unclosed, nested: left as they are.
        assert_eq!(format_doc("\\emph{a{b}}"), "\\emph{a{b}}");
        assert_eq!(format_doc("{\\em a"), "{\\em a");
    }

    #[test]
    fn every_documented_preference_formats() {
        let c = Catalog::new().unwrap();
        let mut left = Vec::new();
        for name in c.prefs.list(false) {
            let (_, long) = c.documentation(&name);
            let f = format_doc(&long);
            // uigtk3's own check: no LaTeX left over.
            if f.contains('\\') {
                left.push(name);
            }
        }
        assert_eq!(left, Vec::<String>::new());
        assert_eq!(capitalize("show it"), "Show it");
        assert_eq!(capitalize(""), "");
    }
}
