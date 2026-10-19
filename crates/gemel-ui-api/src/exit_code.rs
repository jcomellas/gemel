//! Process exit codes shared by all frontends.
//!
//! Ports the exit codes from Unison `src/uicommon.ml`.
//!
//! Derived from Unison, Copyright 1999-2020, Benjamin C. Pierce.

/// How a run ended, with the same numeric values as Unison so that scripts
/// that check `unison`'s exit status keep working.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExitCode {
    /// Everything was synchronized.
    Perfect = 0,
    /// Some items were skipped, but nothing failed.
    Skippy = 1,
    /// There was a non-fatal failure.
    Failed = 2,
    /// A fatal failure occurred.
    Fatal = 3,
}

impl ExitCode {
    /// Combines the outcome of a run, as `Uicommon.exitCode` does.
    pub fn from_outcome(any_skipped: bool, any_failure: bool) -> Self {
        match (any_skipped, any_failure) {
            (_, true) => ExitCode::Failed,
            (true, false) => ExitCode::Skippy,
            (false, false) => ExitCode::Perfect,
        }
    }
}

impl From<ExitCode> for std::process::ExitCode {
    fn from(code: ExitCode) -> Self {
        std::process::ExitCode::from(code as u8)
    }
}

#[cfg(test)]
mod tests {
    use super::ExitCode;

    #[test]
    fn outcome_matches_uicommon() {
        assert_eq!(ExitCode::from_outcome(false, false), ExitCode::Perfect);
        assert_eq!(ExitCode::from_outcome(true, false), ExitCode::Skippy);
        assert_eq!(ExitCode::from_outcome(false, true), ExitCode::Failed);
        assert_eq!(ExitCode::from_outcome(true, true), ExitCode::Failed);
    }
}
