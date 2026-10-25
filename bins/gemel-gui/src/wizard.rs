//! The profile creation assistant's rules: when each page is complete,
//! the second root from what was entered, and what the pages say.
//!
//! Ports `createProfile` in `uigtk3.ml`. Where its text names Unison, the
//! program, it says Gemel, and the command to start a server is
//! `gemel -socket xxx`.

use gemel_ui_api::clroot::{self, Clroot};

/// How the second replica is reached (the "Synchronization kind").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// On this machine.
    Local,
    /// Over ssh.
    Ssh,
    /// On a TCP socket (or a Unix one, `{path}`).
    Socket,
}

impl Kind {
    /// The kind at `i` in the combo box.
    pub fn of(i: i32) -> Kind {
        match i {
            1 => Kind::Ssh,
            2 => Kind::Socket,
            _ => Kind::Local,
        }
    }

    /// What the kind is (`descrLabel`).
    pub fn description(self) -> &'static str {
        match self {
            Kind::Local => "Local synchronization.",
            Kind::Ssh => {
                "This is the recommended way to synchronize with a remote machine.  \
                 A\u{a0}remote instance of Gemel is automatically started via SSH."
            }
            Kind::Socket => {
                "Synchronization with a remote machine by connecting to an instance of \
                 Gemel already listening on a specific TCP port."
            }
        }
    }

    /// What it needs (`requirementLabel`).
    pub fn requirement(self) -> &'static str {
        match self {
            Kind::Local => "",
            Kind::Ssh => {
                "There must be an SSH client installed on this machine, and Gemel and an \
                 SSH server installed on the remote machine."
            }
            Kind::Socket => {
                "There must be a Gemel server running on the remote machine, listening on \
                 the port that you specify here.  (Use \"gemel -socket xxx\" on the remote \
                 machine to start the Gemel server.)"
            }
        }
    }

    /// What to enter (`connDescLabel`).
    pub fn connection(self) -> &'static str {
        match self {
            Kind::Local => "",
            Kind::Ssh => {
                "Please enter the host to connect to and a user name, if different from \
                 your user name on this machine."
            }
            Kind::Socket => "Please enter the host and port to connect to.",
        }
    }
}

/// Whether the replicas are directories or files (`dirKind`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DirKind {
    /// Directories.
    Dir,
    /// Files.
    File,
}

impl DirKind {
    /// The kind at `i` in the combo box.
    pub fn of(i: i32) -> DirKind {
        if i == 1 { DirKind::File } else { DirKind::Dir }
    }

    /// The first one's label.
    pub fn first_label(self, kind: Kind) -> &'static str {
        match (kind == Kind::Local, self) {
            (true, DirKind::Dir) => "First directory:",
            (false, DirKind::Dir) => "Local directory:",
            (true, DirKind::File) => "First file:",
            (false, DirKind::File) => "Local file:",
        }
    }

    /// The second one's label: chosen locally, or entered for the remote
    /// machine.
    pub fn second_label(self, kind: Kind) -> &'static str {
        match (kind == Kind::Local, self) {
            (true, DirKind::Dir) => "Second directory:",
            (true, DirKind::File) => "Second file:",
            (false, DirKind::Dir) => "Remote directory:",
            (false, DirKind::File) => "Remote file:",
        }
    }

    /// Where a remote one is (`secondDirLabel1`, `secondDirLabel2`).
    pub fn remote_note(self, kind: Kind) -> &'static str {
        match (kind, self) {
            (Kind::Local, _) => "",
            (Kind::Ssh, DirKind::Dir) => {
                "The second directory is relative to your home directory on the remote \
                 machine."
            }
            (Kind::Ssh, DirKind::File) => {
                "The second file is relative to your home directory on the remote machine."
            }
            (Kind::Socket, DirKind::Dir) => {
                "The second directory is relative to the working directory of the Gemel \
                 server running on the remote machine."
            }
            (Kind::Socket, DirKind::File) => {
                "The second file is relative to the working directory of the Gemel server \
                 running on the remote machine."
            }
        }
    }
}

/// The description page: a name, of a profile that doesn't exist.
pub fn description_complete(name: &str, exists: bool) -> bool {
    !name.is_empty() && !exists
}

/// `isInteger`: digits, and nothing else.
fn is_integer(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

/// The connection page: local, or a host, with a port for a TCP socket
/// and none otherwise (a Unix socket, `{path}`, or ssh).
pub fn connection_complete(kind: Kind, host: &str, port: &str) -> bool {
    let tcp = kind == Kind::Socket && !host.is_empty() && !host.starts_with('{');
    kind == Kind::Local
        || (!host.is_empty() && ((tcp && is_integer(port)) || (!tcp && port.is_empty())))
}

/// The directory page: remote, or two different local ones.
pub fn directories_complete(kind: Kind, first: &str, second: &str) -> bool {
    kind != Kind::Local || first != second
}

/// `makeRemoteRoot`: the second root, or what is wrong with it.
pub fn second_root(
    kind: Kind,
    second: &str,
    host: &str,
    user: &str,
    port: &str,
) -> Result<String, String> {
    let second = Some(second.trim().to_owned());
    let host = host.trim().to_owned();
    let user = Some(user.trim().to_owned()).filter(|u| !u.is_empty());
    let root = match kind {
        Kind::Local => Clroot::ConnectLocal(second),
        Kind::Ssh => Clroot::ConnectByShell {
            shell: "ssh".to_owned(),
            host,
            user,
            port: None,
            path: second,
        },
        Kind::Socket => Clroot::ConnectBySocket {
            host,
            port: port.to_owned(),
            path: second,
        },
    };
    let problem =
        |e: String| format!("There was a problem with the remote root data you entered.\n\n{e}");
    let root = clroot::fix_host(root)
        .map_err(|e| problem(e.to_string()))?
        .to_string();
    clroot::parse_root(&root).map_err(|e| problem(e.to_string()))?;
    Ok(root)
}

/// The last page's text, as the profile can be made or not.
pub fn conclusion(ok: bool) -> &'static str {
    if ok {
        "You have now finished filling in the profile.\n\nClick \"Apply\" to create it."
    } else {
        "There was an error when preparing the profile.\n\nClick \"Back\" to review what \
         you entered."
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pages_complete_as_uigtk3_has_them() {
        assert!(description_complete("p", false));
        assert!(!description_complete("", false));
        assert!(!description_complete("p", true));

        assert!(connection_complete(Kind::Local, "", "x"));
        assert!(!connection_complete(Kind::Ssh, "", ""));
        assert!(connection_complete(Kind::Ssh, "h", ""));
        // A port left over from the socket's page.
        assert!(!connection_complete(Kind::Ssh, "h", "22"));
        assert!(connection_complete(Kind::Socket, "h", "2000"));
        assert!(!connection_complete(Kind::Socket, "h", ""));
        assert!(!connection_complete(Kind::Socket, "h", "20x"));
        assert!(!connection_complete(Kind::Socket, "h", "+20"));
        // A Unix socket has no port.
        assert!(connection_complete(Kind::Socket, "{/tmp/s}", ""));
        assert!(!connection_complete(Kind::Socket, "{/tmp/s}", "1"));

        assert!(directories_complete(Kind::Local, "/a", "/b"));
        assert!(!directories_complete(Kind::Local, "/a", "/a"));
        assert!(directories_complete(Kind::Local, "", "/b"));
        assert!(directories_complete(Kind::Ssh, "/a", "/a"));
    }

    #[test]
    fn second_roots_as_uigtk3_makes_them() {
        assert_eq!(second_root(Kind::Local, " /b ", "", "", "").unwrap(), "/b");
        assert_eq!(
            second_root(Kind::Ssh, "dir", " host ", " me ", "22").unwrap(),
            "ssh://me@host/dir"
        );
        assert_eq!(
            second_root(Kind::Ssh, "dir", "host", "", "").unwrap(),
            "ssh://host/dir"
        );
        assert_eq!(
            second_root(Kind::Socket, "dir", "host", "", "2000").unwrap(),
            "socket://host:2000/dir"
        );
        // The port isn't checked here, as in OCaml; a host is.
        assert_eq!(
            second_root(Kind::Socket, "dir", "host", "", "x").unwrap(),
            "socket://host:x/dir"
        );
        assert_eq!(
            second_root(Kind::Ssh, "dir", "a b", "", "").unwrap_err(),
            "There was a problem with the remote root data you entered.\n\n\
             ill-formed host specification a b"
        );
        assert!(conclusion(true).contains("Apply") && conclusion(false).contains("Back"));
    }
}
