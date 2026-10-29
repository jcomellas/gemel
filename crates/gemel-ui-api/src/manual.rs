//! Unison's manual (`Strings.docs`), as data: the sections `-doc` prints,
//! and that the graphical interface's documentation window and Help menu
//! show.
//!
//! Ports Unison `src/strings.ml`. Its sections are extracted from Unison's
//! sources into `manual/docs.rs` by `tools/ocaml-fixtures/gen_manual.ml`.
//! The manual is Unison's, as it is, but for its first section, About,
//! which names this program and its version rather than Unison 2.54.0.

mod docs;

/// A section of the manual.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Section {
    /// The topic that names it (`-doc <topic>`); empty for a section that
    /// isn't listed, such as the Overview.
    pub short: &'static str,
    /// Its title.
    pub name: &'static str,
    /// Its text, laid out on 80 columns, a heading first.
    pub text: &'static str,
}

/// The About section, for this program.
static ABOUT: Section = Section {
    short: "about",
    name: "About Gemel",
    text: concat!(
        "Gemel File Synchronizer\nVersion ",
        env!("CARGO_PKG_VERSION"),
        "\n\n"
    ),
};

/// The sections, in order.
pub fn sections() -> impl Iterator<Item = &'static Section> {
    docs::DOCS
        .iter()
        .map(|s| if s.short == ABOUT.short { &ABOUT } else { s })
}

/// The first section that `topic` names, as `Safelist.assoc` finds it.
pub fn find(topic: &str) -> Option<&'static Section> {
    sections().find(|s| s.short == topic)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MY_VERSION;

    #[test]
    fn the_sections_are_unisons_but_for_about() {
        let shorts: Vec<&str> = sections().map(|s| s.short).collect();
        assert_eq!(
            shorts,
            [
                "about",
                "",
                "",
                "people",
                "obtaining",
                "development",
                "copying",
                "ack",
                "upgrading",
                "tutorial",
                "basics",
                "failures",
                "",
                "running",
                "",
            ]
        );
        let about = find("about").unwrap();
        assert_eq!(about.name, "About Gemel");
        assert_eq!(
            about.text,
            format!("Gemel File Synchronizer\nVersion {MY_VERSION}\n\n")
        );
        // What it stands for.
        assert_eq!(docs::DOCS[0].name, "About Unison");
        // The first section with no topic.
        assert_eq!(find("").unwrap().name, "Overview");
        assert!(find("nosuch").is_none());
        let running = find("running").unwrap();
        assert_eq!(running.name, "Running Unison");
        assert!(running.text.starts_with("Running Unison\n\n"));
    }
}
