//! OS layer: filesystem access, properties, xattrs, ACLs, macOS metadata, fingerprints.
//!
//! Platform-specific filesystem access, and the metadata values Unison
//! records about files. This is the only crate that uses `unsafe`, each
//! use in a function marked `#[allow(unsafe_code)]`: `utimensat`, user
//! and group names, `getattrlist`/`setattrlist` on macOS, `umask`,
//! `gethostname`, the Windows file-information calls behind `stat`, and
//! for copying `clonefile` on macOS and `ioctl(FICLONE)`,
//! `copy_file_range` and `sendfile64` on Linux, and for the text interface
//! the terminal's modes (`tcgetattr`/`tcsetattr`, the Windows console's)
//! and catching signals (`sigaction`, `SetConsoleCtrlHandler`), and for
//! the file watcher, Windows' long path names (`GetLongPathNameW`), for
//! sockets `getaddrinfo`, and for ssh under a pseudo-terminal `openpty`,
//! `fcntl` and the child's session (`setsid`, `ioctl(TIOCSCTTY)`, its
//! terminal's modes), and sending a child `SIGTERM`, and on Windows
//! pseudo-consoles (`CreatePseudoConsole`) and starting a process on one
//! (`CreateProcessW` with its attributes, `SearchPathW`, pipes, and waiting
//! for and ending it), and extended attributes (`getxattr` and the rest)
//! and macOS's ACLs (`acl_get_file` and the rest, declared in [`acl`]).
//!
//! Not ported yet: AppleDouble files (resource forks on volumes without
//! them), Windows' ACLs, and block cloning on Windows (see [`reflink`]).

pub mod acl;
#[cfg(windows)]
pub mod conpty;
pub mod fileinfo;
pub mod fingerprint;
pub mod fs;
pub mod fspath;
pub mod net;
pub mod os;
pub mod osx;
pub mod props;
pub mod propsdata;
#[cfg(unix)]
pub mod pty;
pub mod reflink;
pub mod signal;
pub mod system;
pub mod terminal;
pub mod xattr;
