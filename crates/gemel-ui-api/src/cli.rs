//! The preferences of the text interface and of the program's startup.
//!
//! Ports the preferences of Unison `src/uitext.ml` and `src/main.ml`.
//!
//! Derived from Unison, Copyright 1999-2020, Benjamin C. Pierce.
//!
//! They are here, not in the `gemel` binary, because every host registers
//! every preference: a server refuses one it doesn't know, and `silent`
//! and `dumbtty` are sent to it.

use gemel_base::prefs::{
    BoolWithDefault, Category, DecodeError, Input, Internal, Kind, PrefDef, PrefValue, Prefs, Topic,
};
use gemel_base::util::Error;
use gemel_compat::umarshal::{Reader, Writer};

// ---- `uitext` ----

/// `dumbtty`: on by default inside Emacs.
pub static DUMBTTY: PrefDef<bool> = PrefDef::new(
    "dumbtty",
    Category::Advanced(Topic::CLI),
    || std::env::var_os("EMACS").is_some_and(|v| !v.is_empty()),
    "do not change terminal settings in text UI",
    "When set to \\verb|true|, this flag makes the text mode user interface \
     avoid trying to change any of the terminal settings.  (Normally, Unison \
     puts the terminal in `raw mode', so that it can do things like \
     overwriting the current line.) This is useful, for example, when Unison \
     runs in a shell inside of Emacs.  \n\nWhen \\verb|dumbtty| is set, \
     commands to the user interface need to be followed by a carriage return \
     before Unison will execute them.  (When it is off, Unison recognizes \
     keystrokes as soon as they are typed.)\n\nThis preference has no effect \
     on the graphical user interface.",
);

/// `silent`.
pub static SILENT: PrefDef<bool> = PrefDef::new(
    "silent",
    Category::Basic(Topic::Syncprocess_CLI),
    || false,
    "print nothing except error messages",
    "When this preference is set to {\\tt true}, the textual user interface \
     will print nothing at all, except in the case of errors.  Setting \
     \\texttt{silent} to true automatically sets the \\texttt{batch} \
     preference to {\\tt true}.",
);

/// `color` (`colorMode`).
pub static COLOR: PrefDef<BoolWithDefault> = PrefDef::new(
    "color",
    Category::Advanced(Topic::CLI),
    || BoolWithDefault::Default,
    "use color output for text UI (true/false/default)",
    "When set to {\\tt true}, this flag enables color output in text mode \
     user interface. When set to {\\tt false}, all color output is disabled. \
     Default is to enable color if the {\\tt NO\\_COLOR} environment \
     variable is not set.",
)
.local();

/// `i` (`profmgrPref`).
pub static PROFILE_MANAGER: PrefDef<bool> = PrefDef::new(
    "i",
    Category::Basic(Topic::CLI),
    || false,
    "interactive profile mode (text UI); command-line only",
    "Provide this preference in the command line arguments to enable \
     interactive profile manager in the text user interface. Currently only \
     profile listing and interactive selection are available. Preferences \
     like \\texttt{batch} and \\texttt{silent} remain applicable to \
     synchronization functionality.",
)
.cli_only();

// ---- `main` ----

/// `version` (`printVersionAndExit`).
pub static VERSION: PrefDef<bool> = PrefDef::new(
    "version",
    Category::Basic(Topic::General),
    || false,
    "print version and exit",
    "Print the current version number and exit.  (This option only makes \
     sense on the command line.)",
)
.cli_only();

/// `doc` (`docs`).
pub static DOC: PrefDef<String> = PrefDef::new(
    "doc",
    Category::Basic(Topic::General),
    String::new,
    "show documentation ('-doc topics' lists topics)",
    "The command-line argument \\texttt{-doc \\ARG{secname}} causes unison to \
     display section  \\ARG{secname} of the manual on the standard output and \
     then exit.   Use \\verb|-doc all| to display the whole manual, which \
     includes exactly the same information as the printed and HTML manuals, \
     modulo formatting.  Use \\verb|-doc topics| to obtain a list of the \
     names of the various sections that can be printed.",
)
.cli_only();

/// `prefsdocs`.
pub static PREFS_DOCS: PrefDef<bool> = PrefDef::new(
    "prefsdocs",
    Category::Internal(Internal::Devel),
    || false,
    "*show full documentation for all preferences (and then exit)",
    "",
)
.cli_only();

/// `prefsman`.
pub static PREFS_MAN: PrefDef<String> = PrefDef::new(
    "prefsman",
    Category::Internal(Internal::Devel),
    String::new,
    "*show manpage documentation for all preferences (and then exit)",
    "",
)
.cli_only();

/// `server`.
pub static SERVER: PrefDef<bool> = PrefDef::new(
    "server",
    Category::Internal(Internal::Other),
    || false,
    "*normal or server mode",
    "",
)
.cli_only();

/// `socket`.
pub static SOCKET: PrefDef<String> = PrefDef::new(
    "socket",
    Category::Advanced(Topic::Remote),
    String::new,
    "act as a server on a socket",
    "Start gemel as a server listening on a TCP socket (with TCP port number \
     as argument) or a local socket (aka Unix domain socket) (with socket \
     path as argument).",
)
.cli_only();

/// `listen` (`serverHost`), also `host`.
pub static LISTEN: PrefDef<String> = PrefDef::new(
    "listen",
    Category::Advanced(Topic::Remote),
    String::new,
    "listen on this name or addr in server socket mode (can repeat)",
    "When acting as a server on a TCP socket, Unison will by default listen \
     on \"any\" address (0.0.0.0 and [::]).  This command-line argument \
     allows to specify a different listening address and can be repeated to \
     listen on multiple addresses.  Listening address can be specified as a \
     host name or an IP address.",
)
.cli_only();

/// Which user interface to use (`Uicommon.interface`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Interface {
    /// The text interface.
    Text,
    /// The graphical interface.
    Graphic,
}

impl PrefValue for Interface {
    const KIND: Kind = Kind::Custom;
    fn apply(&self, input: Input<'_>) -> Result<Self, String> {
        match input {
            Input::Str("text") => Ok(Interface::Text),
            Input::Str("graphic") => Ok(Interface::Graphic),
            Input::Str(other) => Err(format!(
                "option ui :\ntext -> textual user interface\ngraphic -> graphic user \
                 interface\n{other} is not a legal value"
            )),
            _ => Err("ui takes a string".to_owned()),
        }
    }
    fn print(&self) -> Vec<String> {
        vec![
            match self {
                Interface::Text => "text",
                Interface::Graphic => "graphic",
            }
            .to_owned(),
        ]
    }
    /// `minterface`: `sum2 unit unit`.
    fn marshal(&self, w: &mut Writer) {
        w.tag(match self {
            Interface::Text => 0,
            Interface::Graphic => 1,
        });
    }
    fn unmarshal(r: &mut Reader<'_>) -> Result<Self, DecodeError> {
        Ok(match r.tag(2)? {
            0 => Interface::Text,
            _ => Interface::Graphic,
        })
    }
}

/// `ui` (`interface`).
pub static UI: PrefDef<Interface> = PrefDef::new(
    "ui",
    Category::Advanced(Topic::General),
    || Interface::Graphic,
    "select UI ('text' or 'graphic'); command-line only",
    "This preference selects either the graphical or the textual user \
     interface.  Legal values are \\verb|graphic| or \\verb|text|.  \n\n\
     Because this option is processed specially during Unison's start-up \
     sequence, it can {\\em only} be used on the command line.  In \
     preference files it has no effect.\n\nIf the Unison executable was \
     compiled with only a textual interface, this option has no effect.  \
     (The pre-compiled binaries are all compiled with both interfaces \
     available.)",
)
.cli_only();

/// Registers `uitext`'s preferences, then `main`'s, in OCaml's order.
pub fn register_prefs(prefs: &mut Prefs) -> Result<(), Error> {
    prefs.register(&DUMBTTY)?;
    prefs.register(&SILENT)?;
    prefs.register(&COLOR)?;
    prefs.register(&PROFILE_MANAGER)?;
    prefs.register(&VERSION)?;
    prefs.register(&DOC)?;
    prefs.register(&PREFS_DOCS)?;
    prefs.register(&PREFS_MAN)?;
    prefs.register(&SERVER)?;
    prefs.register(&SOCKET)?;
    prefs.register(&LISTEN)?;
    prefs.alias(&LISTEN, "host")?;
    prefs.register(&UI)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_interface_is_text_or_graphic() {
        let mut prefs = crate::new_prefs().unwrap();
        prefs
            .parse_cmd_line(&["-ui".to_owned(), "text".to_owned()])
            .unwrap();
        assert_eq!(*prefs.read(&UI), Interface::Text);
        let e = prefs
            .parse_cmd_line(&["-ui".to_owned(), "tui".to_owned()])
            .unwrap_err();
        assert!(e.to_string().ends_with("tui is not a legal value"), "{e}");
        // `host` is `listen`.
        prefs
            .parse_cmd_line(&["-host".to_owned(), "localhost".to_owned()])
            .unwrap();
        assert_eq!(prefs.read(&LISTEN), "localhost");
    }
}
