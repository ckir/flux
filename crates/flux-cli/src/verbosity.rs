// Copyright (c) 2026 Costas Kirgoussios
//
// This software is licensed under the PolyForm Noncommercial License 1.0.0.
// You may obtain a copy of the License at:
// https://polyformproject.org/licenses/noncommercial/1.0.0/

//! How much `flux` says: the `-v` count and `--quiet`, resolved once in `main` and kept in
//! a process-global slot so the report code can read it without it being threaded through.

use std::sync::OnceLock;

/// The resolved `-v` count and `--quiet`, never both.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Verbosity {
    /// 0 to 3: how many `-v` were given, clamped.
    pub level: u8,
    /// `--quiet`: print only errors, the summary line and the dry-run notes.
    pub quiet: bool,
}

impl Verbosity {
    /// Resolves the flags; `-v` and `--quiet` together are a usage error.
    pub fn from_flags(verbose: u8, quiet: bool) -> Result<Verbosity, String> {
        if verbose > 0 && quiet {
            return Err("-v/--verbose and -q/--quiet cannot be used together".to_string());
        }
        Ok(Verbosity { level: verbose.min(3), quiet })
    }

    /// Whether the informational notes are printed: not under `--quiet`, except a dry run.
    pub fn shows_notes(self, dry_run: bool) -> bool {
        !self.quiet || dry_run
    }
}

static CURRENT: OnceLock<Verbosity> = OnceLock::new();

/// Records the process's verbosity; a second call is ignored.
pub fn set(v: Verbosity) {
    let _ = CURRENT.set(v);
}

/// The recorded verbosity, or the default (level 0, not quiet) when none was set.
pub fn get() -> Verbosity {
    CURRENT.get().copied().unwrap_or(Verbosity { level: 0, quiet: false })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_flags_clamps_to_three() {
        for (given, level) in [(0, 0), (1, 1), (2, 2), (3, 3), (4, 3), (255, 3)] {
            let v = Verbosity::from_flags(given, false).unwrap();
            assert_eq!(v, Verbosity { level, quiet: false }, "-v x{given}");
        }
        assert_eq!(Verbosity::from_flags(0, true).unwrap(), Verbosity { level: 0, quiet: true });
    }

    #[test]
    fn from_flags_rejects_both() {
        for given in [1, 2, 4] {
            assert_eq!(
                Verbosity::from_flags(given, true),
                Err("-v/--verbose and -q/--quiet cannot be used together".to_string())
            );
        }
    }

    #[test]
    fn shows_notes_truth_table() {
        let loud = Verbosity { level: 0, quiet: false };
        let quiet = Verbosity { level: 0, quiet: true };
        assert!(loud.shows_notes(false));
        assert!(loud.shows_notes(true));
        assert!(!quiet.shows_notes(false));
        assert!(quiet.shows_notes(true));
    }
}
