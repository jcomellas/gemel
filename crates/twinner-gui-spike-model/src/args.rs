//! Command-line flags shared by both spike binaries.

use std::time::Duration;

/// Options for a spike run.
#[derive(Clone, Debug, PartialEq)]
pub struct SpikeArgs {
    /// Number of recon rows to generate.
    pub rows: usize,
    /// Progress updates per second sent by the fake engine.
    pub rate: u32,
    /// When set, auto-scroll the table and run the feed for this long, print
    /// frame statistics and exit.
    pub bench: Option<Duration>,
    /// Exit as soon as the first frame has been presented (startup timing).
    pub exit_after_first_frame: bool,
    /// With `--bench`: run without a window and time the main-thread work
    /// per frame instead (see [`crate::stats::headless_bench`]).
    pub headless: bool,
}

impl Default for SpikeArgs {
    fn default() -> Self {
        SpikeArgs {
            rows: 50_000,
            rate: 500,
            bench: None,
            exit_after_first_frame: false,
            headless: false,
        }
    }
}

impl SpikeArgs {
    /// Parses `--rows N`, `--rate N`, `--bench SECS`, `--headless` and
    /// `--exit-after-first-frame`.
    pub fn parse<I: IntoIterator<Item = String>>(args: I) -> Result<Self, String> {
        let mut out = SpikeArgs::default();
        let mut it = args.into_iter();
        while let Some(arg) = it.next() {
            let mut value = |name: &str| {
                it.next()
                    .ok_or_else(|| format!("{name} needs a value"))
                    .and_then(|v| v.parse::<f64>().map_err(|e| format!("{name}: {e}")))
            };
            match arg.as_str() {
                "--rows" => out.rows = value("--rows")? as usize,
                "--rate" => out.rate = value("--rate")? as u32,
                "--bench" => out.bench = Some(Duration::from_secs_f64(value("--bench")?)),
                "--exit-after-first-frame" => out.exit_after_first_frame = true,
                "--headless" => out.headless = true,
                other => return Err(format!("unknown argument {other:?}")),
            }
        }
        Ok(out)
    }

    /// Parses the process arguments, exiting with a usage message on error.
    pub fn from_env() -> Self {
        Self::parse(std::env::args().skip(1)).unwrap_or_else(|e| {
            eprintln!("error: {e}");
            eprintln!(
                "usage: [--rows N] [--rate UPDATES_PER_SEC] [--bench SECS [--headless]] \
                 [--exit-after-first-frame]"
            );
            std::process::exit(2);
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_flags() {
        let args = ["--rows", "10", "--rate", "5000", "--bench", "2.5"].map(String::from);
        let parsed = SpikeArgs::parse(args).unwrap();
        assert_eq!(parsed.rows, 10);
        assert_eq!(parsed.rate, 5000);
        assert_eq!(parsed.bench, Some(Duration::from_millis(2500)));
        assert!(!parsed.exit_after_first_frame);
        assert!(SpikeArgs::parse(["--nope".to_string()]).is_err());
    }
}
