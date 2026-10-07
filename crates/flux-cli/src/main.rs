//! Flux CLI (spec §3.6): argument parsing and command dispatch. The logic lives in the
//! `flux_cli` library (`resolve`, `report`, `exit_code`), where it is unit-tested.

use clap::{Args, Parser, Subcommand, ValueEnum};
use flux_cli::exit_code;
use flux_cli::report::{self, Report};
use flux_cli::resolve::{self, Job, Stop};
use flux_core::run::RunConfig;
use flux_fs::{
    Code, CopyOptions, Durability, ExistingPolicy, OperationId, Preserve, Publish, Safety,
};
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
    /// What happens to a file that already exists at DEST is chosen by one of
    /// --overwrite (the default: replace it), --update (replace it only when the source
    /// is newer) or --skip-existing (leave it untouched); at most one may be given. A
    /// skipped file is not a failure. A symlink given as SOURCE is not followed. Several
    /// sources are not supported yet.
    Copy(CopyArgs),
}

#[derive(Args)]
#[command(group(clap::ArgGroup::new("existing").args(["overwrite", "update", "skip_existing"]).multiple(false)))]
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
    /// Supersede every resumable prior operation on DEST: its state is marked ABANDONED and its partial files are
    /// deleted, then this copy runs.
    #[arg(long)]
    restart: bool,
    /// With --restart: take over a lock that a crashed run left empty or unreadable (§240.5).
    #[arg(long, requires = "restart")]
    break_lock: bool,
    /// Replace a file that already exists at DEST (the default; §5.1).
    #[arg(long)]
    overwrite: bool,
    /// Replace an existing file at DEST only when the source is newer (§5.1).
    #[arg(long)]
    update: bool,
    /// Leave an existing file at DEST untouched (§5.1).
    #[arg(long)]
    skip_existing: bool,
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
        // A placeholder: the run replaces it with its own operation id (`RunConfig::operation_id`).
        operation_id: OperationId::new(String::new()),
        existing: if args.update {
            ExistingPolicy::Update
        } else if args.skip_existing {
            ExistingPolicy::SkipExisting
        } else {
            ExistingPolicy::Overwrite
        },
    }
}

/// The run's configuration: two fresh ids for this invocation (the operation and this process, cut 7a), the boot
/// session, and §101's heartbeat interval; in a debug build, the crash hook `debug_hook` and the interval override
/// `heartbeat_interval` read from the environment.
fn run_config(args: &CopyArgs) -> RunConfig {
    RunConfig {
        restart: args.restart,
        break_lock: args.break_lock,
        operation_id: flux_core::ids::new_id(),
        owner_instance_id: flux_core::ids::new_id(),
        boot_session_id: flux_platform::boot_session_id(),
        before_mutation: debug_hook(),
        heartbeat_interval: heartbeat_interval(),
    }
}

/// Part 3b-2, decision 2 (debug builds only): with `FLUX_TEST_STALL_AT=<n>` and `FLUX_TEST_STALL_FILE=<path>`, the
/// run creates `<path>` and then blocks forever just before its n-th guarded mutation, so an end-to-end test can kill
/// it at a known point. A release build has no hook.
#[cfg(debug_assertions)]
fn debug_hook() -> Option<flux_core::run::BeforeMutation> {
    let at: u64 = std::env::var("FLUX_TEST_STALL_AT").ok()?.parse().ok()?;
    let file = PathBuf::from(std::env::var_os("FLUX_TEST_STALL_FILE")?);
    let seen = std::sync::atomic::AtomicU64::new(0);
    Some(std::sync::Arc::new(move || {
        if seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1 == at {
            let _ = std::fs::write(&file, b"stalled");
            loop {
                std::thread::sleep(std::time::Duration::from_secs(60));
            }
        }
    }))
}

#[cfg(not(debug_assertions))]
fn debug_hook() -> Option<flux_core::run::BeforeMutation> {
    None
}

/// §101's 5 s (cut 7b). In a debug build, `FLUX_TEST_HEARTBEAT_INTERVAL_MS=<n>` replaces it, so an end-to-end test can
/// watch the lock record's heartbeat move. A release build has no override.
#[cfg(debug_assertions)]
fn heartbeat_interval() -> std::time::Duration {
    std::env::var("FLUX_TEST_HEARTBEAT_INTERVAL_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .map(std::time::Duration::from_millis)
        .unwrap_or(flux_core::run::HEARTBEAT_INTERVAL)
}

#[cfg(not(debug_assertions))]
fn heartbeat_interval() -> std::time::Duration {
    flux_core::run::HEARTBEAT_INTERVAL
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
    let cfg = run_config(args);
    let fs = flux_platform::StdFileSystem;
    match job {
        Job::Tree { src, dst } => {
            let started = Instant::now();
            let run = flux_core::run::tree(&fs, &src, &dst, &opts, &cfg, &mut |f| {
                for line in report::record_lines(&f) {
                    err(&line);
                }
            });
            let ms = millis(started);
            let rep = Report::tree_run(&run, ms);
            match &run.copy {
                Some(result) => {
                    let outcome = match result {
                        Ok(out) => out,
                        Err(a) => {
                            err(&a.error.to_string());
                            &a.outcome
                        }
                    };
                    for line in report::tree_warning_lines(outcome) {
                        err(&line);
                    }
                    lines(report::run_lines(&run));
                    err(&report::summary_line(&rep, outcome.directories_created));
                }
                None => lines(report::run_lines(&run)),
            }
            json(args, &rep);
            exit_code::for_tree_run(&run)
        }
        Job::File { src, dst, target_existed } => {
            let started = Instant::now();
            let run = flux_core::run::file(&fs, &src, &dst, &opts, &cfg);
            let ms = millis(started);
            let rep = Report::file_run(&run, target_existed, ms);
            match &run.copy {
                Some(result) => {
                    match result {
                        Ok(o) => {
                            let target = dst.display().to_string();
                            for m in &o.metadata_failures {
                                err(&report::complaint_line(&target, m));
                            }
                            if let Some(w) = o
                                .identity_degraded
                                .as_ref()
                                .and_then(|d| report::file_warning(d, &dst))
                            {
                                err(&w);
                            }
                        }
                        // `CopyError`'s Display is "CODE: source" plus a staging temporary that
                        // could not be removed - the one thing the user needs to clean up.
                        Err(e) => err(&e.to_string()),
                    }
                    lines(report::run_lines(&run));
                    err(&report::summary_line(&rep, 0));
                }
                None => lines(report::run_lines(&run)),
            }
            json(args, &rep);
            exit_code::for_file_run(&run)
        }
    }
}

/// Each line to stderr.
fn lines(v: Vec<String>) {
    for line in v {
        err(&line);
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
    fn existing_policy_defaults_to_overwrite() {
        assert_eq!(options(&parse(&[])).existing, ExistingPolicy::Overwrite);
        assert_eq!(options(&parse(&["--overwrite"])).existing, ExistingPolicy::Overwrite);
    }

    #[test]
    fn update_and_skip_existing_reach_the_options() {
        assert_eq!(options(&parse(&["--update"])).existing, ExistingPolicy::Update);
        assert_eq!(options(&parse(&["--skip-existing"])).existing, ExistingPolicy::SkipExisting);
    }

    #[test]
    fn giving_two_policies_is_a_usage_error() {
        let err = Cli::try_parse_from(["flux", "copy", "a", "b", "--update", "--skip-existing"])
            .err()
            .expect("two policies must be rejected");
        assert_eq!(err.exit_code(), 2);
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

    #[test]
    fn break_lock_needs_restart_and_each_run_gets_fresh_ids() {
        assert!(Cli::try_parse_from(["flux", "copy", "a", "b", "--break-lock"]).is_err());
        let both = parse(&["--restart", "--break-lock"]);
        assert!(both.restart && both.break_lock);
        let cfg = run_config(&parse(&["--restart"]));
        assert!(cfg.restart && !cfg.break_lock);
        assert!(
            flux_core::ids::is_id(&cfg.operation_id)
                && flux_core::ids::is_id(&cfg.owner_instance_id)
        );
        assert_ne!(cfg.operation_id, cfg.owner_instance_id);
        assert_ne!(run_config(&parse(&[])).operation_id, cfg.operation_id, "one id per invocation");
    }
}
