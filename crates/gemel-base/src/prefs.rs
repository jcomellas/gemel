//! Preferences and `.prf` profile parsing (file-format compatible).
//!
//! Ports Unison `src/ubase/prefs.ml`.
//!
//! Derived from Unison, Copyright 1999-2002.

/// Value of a three-state boolean preference (`createBoolWithDefault`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BoolWithDefault {
    /// `true` (or `yes`).
    True,
    /// `false` (or `no`).
    False,
    /// `default` (or `auto`): let Unison decide.
    #[default]
    Default,
}
