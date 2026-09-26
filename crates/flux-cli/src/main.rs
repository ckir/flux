//! Flux CLI (spec §3.6): argument parsing and command dispatch. The logic lives in the
//! `flux_cli` library (`resolve`, `report`, `exit_code`), where it is unit-tested.

use clap::{Args, Parser, Subcommand, ValueEnum};
use flux_cli::exit_code;
use flux_cli::report::{self, Report};
use flux_cli::resolve::{self, Job, Stop};
use flux_fs::{Code, CopyOptions, Durability, OperationId, Preserve, Publish, Safety};
use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

#[derive(Parser)]
#[command(name = "flux", version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Copy a file, or a folder's contents, to DEST (§4.1).
    ///
    /// A folder copy never replaces an existing file at DEST: each one is reported as
    /// DESTINATION_NAMESPACE_COLLISION and left intact (exit 1) until replacement is
    /// supported. A symlink given as SOURCE is not followed. Several sources are not
    /// supported yet.
    Copy(CopyArgs),
}

#[derive(Args)]
struct CopyArgs {
    source: PathBuf,
    destination: PathBuf,
    /// Fail a file if its timestamps cannot be applied (§44.1).
    #[arg(long)]
    preserve_times: bool,
    /// Fail a file if its permissions cannot be applied (§44.1).
    #[arg(long)]
    preserve_permissions: bool,
    /// Both --preserve-times and --preserve-permissions.
    #[arg(long)]
    preserve: bool,
    /// How durably each published file is written (§141, §165).
    #[arg(long, value_enum, default_value_t = DurabilityArg::Normal)]
    durability: DurabilityArg,
    /// `strict` refuses whenever source and destination cannot be compared by a strong
    /// filesystem identity, instead of warning and continuing.
    #[arg(long, value_enum, default_value_t = SafetyArg::Default)]
    safety: SafetyArg,
    /// Print the §53 report as JSON on stdout. bytes_total counts copied bytes only:
    /// the size of a file that failed is not known.
    #[arg(long)]
    json: bool,
}

#[derive(Clone, Copy, ValueEnum)]
enum DurabilityArg {
    Normal,
    Strict,
}

#[derive(Clone, Copy, ValueEnum)]
enum SafetyArg {
    Default,
    Strict,
}

fn main() -> ExitCode {
    let Commands::Copy(args) = Cli::parse().command;
    ExitCode::from(copy(&args))
}

/// Explicitly requested preservation is Strict; otherwise best effort (§44.1).
fn options(args: &CopyArgs) -> CopyOptions {
    let preserve = |asked: bool| {
        if asked || args.preserve { Preserve::Strict } else { Preserve::Default }
    };
    CopyOptions {
        preserve_times: preserve(args.preserve_times),
        preserve_permissions: preserve(args.preserve_permissions),
        durability: match args.durability {
            DurabilityArg::Normal => Durability::Normal,
            DurabilityArg::Strict => Durability::Strict,
        },
        // The single-file default (§5.1 --overwrite); copy_tree forces NoReplace (§241.5).
        publish: Publish::Replace,
        safety: match args.safety {
            SafetyArg::Default => Safety::Default,
            SafetyArg::Strict => Safety::Strict,
        },
        operation_id: OperationId::new(format!("{}", std::process::id())),
    }
}

fn copy(args: &CopyArgs) -> u8 {
    let job = match resolve::job(&args.source, &args.destination) {
        Ok(job) => job,
        Err(Stop::Usage(msg)) => {
            err(&format!("error: {msg}"));
            return exit_code::USAGE;
        }
        Err(Stop::SymlinkSource) => {
            err(&format!(
                "{}: {}: {}; name its target to copy what it points at",
                Code::SymlinkCreationUnavailable.as_str(),
                args.source.display(),
                report::SYMLINK_WHY
            ));
            json(args, &Report::pre_engine(true));
            return exit_code::FAILED;
        }
        Err(Stop::Failed(line)) => {
            err(&line);
            json(args, &Report::pre_engine(false));
            return exit_code::FAILED;
        }
    };
    let opts = options(args);
    let fs = flux_platform::StdFileSystem;
    match job {
        Job::Tree { src, dst } => {
            let started = Instant::now();
            let result = flux_core::copy_tree(&fs, &src, &dst, &opts, &mut |f| {
                for line in report::record_lines(&f) {
                    err(&line);
                }
            });
            let ms = millis(started);
            let (outcome, aborted) = match &result {
                Ok(out) => (out, false),
                Err(a) => {
                    err(&a.error.to_string());
                    (&a.outcome, true)
                }
            };
            for line in report::warning_lines(&outcome.warnings) {
                err(&line);
            }
            let rep = Report::tree(outcome, aborted, ms);
            err(&report::summary_line(&rep, outcome.directories_created));
            json(args, &rep);
            exit_code::for_tree(&result)
        }
        Job::File { src, dst, target_existed } => {
            let started = Instant::now();
            let result = flux_core::copy_file(&fs, &src, &dst, &opts);
            let ms = millis(started);
            match &result {
                Ok(o) => {
                    let target = dst.display().to_string();
                    for m in &o.metadata_failures {
                        err(&report::complaint_line(&target, m));
                    }
                    if let Some(w) =
                        o.identity_degraded.as_ref().and_then(|d| report::file_warning(d, &dst))
                    {
                        err(&w);
                    }
                }
                // `CopyError`'s Display is "CODE: source" plus a staging temporary that
                // could not be removed - the one thing the user needs to clean up.
                Err(e) => err(&e.to_string()),
            }
            let rep = Report::file(&result, target_existed, ms);
            err(&report::summary_line(&rep, 0));
            json(args, &rep);
            exit_code::for_file(&result)
        }
    }
}

/// stderr, never panicking: a closed pipe must not turn a finished copy into exit 101.
fn err(line: &str) {
    let _ = writeln!(std::io::stderr(), "{line}");
}

fn json(args: &CopyArgs, r: &Report) {
    if args.json {
        let _ = writeln!(std::io::stdout(), "{}", r.to_json());
    }
}

fn millis(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(extra: &[&str]) -> CopyArgs {
        let mut argv = vec!["flux", "copy", "a", "b"];
        argv.extend_from_slice(extra);
        let Commands::Copy(args) = Cli::try_parse_from(argv).unwrap().command;
        args
    }

    #[test]
    fn the_defaults_are_best_effort_normal_and_default_safety() {
        let o = options(&parse(&[]));
        assert_eq!(
            (o.preserve_times, o.preserve_permissions),
            (Preserve::Default, Preserve::Default)
        );
        assert_eq!(o.durability, Durability::Normal);
        assert_eq!(o.safety, Safety::Default);
        assert_eq!(o.publish, Publish::Replace);
    }

    #[test]
    fn safety_strict_reaches_the_options() {
        assert_eq!(options(&parse(&["--safety=strict"])).safety, Safety::Strict);
    }

    #[test]
    fn preserve_sets_both_items_strict_and_each_flag_sets_its_own() {
        let both = options(&parse(&["--preserve"]));
        assert_eq!(
            (both.preserve_times, both.preserve_permissions),
            (Preserve::Strict, Preserve::Strict)
        );
        let times = options(&parse(&["--preserve-times"]));
        assert_eq!(
            (times.preserve_times, times.preserve_permissions),
            (Preserve::Strict, Preserve::Default)
        );
        let perms = options(&parse(&["--preserve-permissions"]));
        assert_eq!(
            (perms.preserve_times, perms.preserve_permissions),
            (Preserve::Default, Preserve::Strict)
        );
    }

    #[test]
    fn durability_strict_reaches_the_options() {
        assert_eq!(options(&parse(&["--durability=strict"])).durability, Durability::Strict);
    }

    #[test]
    fn an_unknown_value_is_a_usage_error() {
        assert!(Cli::try_parse_from(["flux", "copy", "a", "b", "--safety=loose"]).is_err());
    }
}
