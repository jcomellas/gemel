//! Archives and update detection.
//!
//! Ports Unison `src/update.ml`.
//!
//! Derived from Unison, Copyright 1999-2020, Benjamin C. Pierce.

/// On-disk archive format version. Must match `archiveFormat` in Unison's
/// `update.ml`; it appears in archive headers and in archive file names.
pub const ARCHIVE_FORMAT: u32 = 23;
