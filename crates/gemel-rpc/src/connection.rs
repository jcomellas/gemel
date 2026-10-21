//! Establishing connections to servers over ssh/rsh or sockets, and the server loop.
//!
//! Ports Unison `src/remote.ml`.
//!
//! Derived from Unison, Copyright 1999-2020, Benjamin C. Pierce.
//!
//! Only the preferences so far, so that profiles setting them are
//! accepted; the connections come with remote roots (milestone 3).

use gemel_base::prefs::{Category, PrefDef, Prefs, Topic};
use gemel_base::util::Error;

/// `sshcmd`.
pub static SSH_CMD: PrefDef<String> = PrefDef::new(
    "sshcmd",
    Category::Advanced(Topic::Remote),
    || "ssh".to_owned(),
    "path to the ssh executable",
    "This preference can be used to explicitly set the name of the ssh \
     executable (e.g., giving a full path name), if necessary.",
);

/// `sshargs`.
pub static SSH_ARGS: PrefDef<String> = PrefDef::new(
    "sshargs",
    Category::Advanced(Topic::Remote),
    String::new,
    "other arguments (if any) for remote shell command",
    "The string value of this preference will be passed as additional \
     arguments (besides the host name and the name of the Unison executable \
     on the remote system) to the \\verb|ssh| command used to invoke the \
     remote server. The backslash is an escape character.",
);

/// `servercmd`.
pub static SERVER_CMD: PrefDef<String> = PrefDef::new(
    "servercmd",
    Category::Advanced(Topic::Remote),
    String::new,
    "name of gemel executable on remote server",
    "This preference can be used to explicitly set the name of the Unison \
     executable on the remote server (e.g., giving a full path name), if \
     necessary.",
);

/// `addversionno`.
pub static ADD_VERSION_NO: PrefDef<bool> = PrefDef::new(
    "addversionno",
    Category::Advanced(Topic::Remote),
    || false,
    "add version number to name of gemel on server",
    "When this flag is set to {\\tt true}, Unison will use \
     \\texttt{unison-\\ARG{currentmajorversionnumber}} instead of just \
     \\verb|unison| as the remote server command (note that the minor version \
     number is dropped -- e.g., unison-2.51).  This allows multiple binaries \
     for different versions of unison to coexist conveniently on the same \
     server: whichever version is run on the client, the same version will \
     be selected on the server.",
);

/// `stream` (`streamingActivated`).
pub static STREAMING_ACTIVATED: PrefDef<bool> = PrefDef::new(
    "stream",
    Category::Advanced(Topic::Remote),
    || true,
    "use a streaming protocol for transferring file contents",
    "When this preference is set, Unison will use an experimental streaming \
     protocol for transferring file contents more efficiently. The default \
     value is \\texttt{true}.",
)
.deprecated();

/// `halfduplex`.
pub static HALF_DUPLEX: PrefDef<bool> = PrefDef::new(
    "halfduplex",
    Category::Advanced(Topic::Remote),
    || false,
    "force half-duplex communication with the server",
    "When this flag is set to {\\tt true}, Unison network communication is \
     forced to be half duplex (the client and the server never \
     simultaneously emit data).  If you experience unstabilities with your \
     network link, this may help.",
)
.deprecated();

/// `killserver`.
pub static KILL_SERVER: PrefDef<bool> = PrefDef::new(
    "killserver",
    Category::Advanced(Topic::Remote),
    || false,
    "kill server when done (even when using sockets)",
    "When set to \\verb|true|, this flag causes Unison to kill the remote \
     server process when the synchronization is finished.  This behavior is \
     the default for \\verb|ssh| connections, so this preference is not \
     normally needed when running over \\verb|ssh|; it is provided so that \
     socket-mode servers can be killed off after a single run of Unison, \
     rather than waiting to accept future connections.  (Some users prefer \
     to start a remote socket server for each run of Unison, rather than \
     leaving one running all the time.)",
);

/// Registers this module's preferences, in OCaml's order, and the ones it
/// removed (`rshcmd` and `rshargs`, since 2.52).
pub fn register_prefs(prefs: &mut Prefs) -> Result<(), Error> {
    prefs.register(&SSH_CMD)?;
    prefs.register(&SSH_ARGS)?;
    prefs.mark_removed("rshcmd");
    prefs.mark_removed("rshargs");
    prefs.register(&SERVER_CMD)?;
    prefs.register(&ADD_VERSION_NO)?;
    prefs.register(&STREAMING_ACTIVATED)?;
    prefs.register(&HALF_DUPLEX)?;
    prefs.register(&KILL_SERVER)?;
    // For backward compatibility.
    prefs.alias(&KILL_SERVER, "killServer")
}
