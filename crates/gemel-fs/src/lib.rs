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
//! the file watcher, Windows' long path names (`GetLongPathNameW`).
//!
//! Not ported yet: AppleDouble files (resource forks on volumes without
//! them), reading and setting xattrs and ACLs, and block cloning on
//! Windows (see [`reflink`]).

pub mod fileinfo;
pub mod fingerprint;
pub mod fs;
pub mod fspath;
pub mod os;
pub mod osx;
pub mod props;
pub mod reflink;
pub mod signal;
pub mod system;
pub mod terminal;
