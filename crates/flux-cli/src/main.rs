// Copyright (c) 2026 Costas Kirgoussios
//
// This software is licensed under the PolyForm Noncommercial License 1.0.0.
// You may obtain a copy of the License at:
// https://polyformproject.org/licenses/noncommercial/1.0.0/

//! Flux CLI (spec §3.6): argument parsing and command dispatch. The logic lives in the
//! `flux_cli` library (`resolve`, `report`, `exit_code`), where it is unit-tested.

use clap::{ArgAction, Args, Parser, Subcommand, ValueEnum};
use flux_cli::exit_code;
use flux_cli::report::{self, Report};
use flux_cli::resolve::{self, Job, Stop};
use flux_cli::verify_report;
use flux_core::run::RunConfig;
use flux_core::verify::VerifyError;
use flux_fs::{
    Code, CopyOptions, Durability, ExistingPolicy, Links, OperationId, Preserve, Publish, Safety,
};
use std::io::Write;
use std::path::{Path, PathBuf};
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
    /// Copy SOURCE to DEST, or several sources into the folder DEST (§4.1).
    ///
    /// With one SOURCE, a file is copied to DEST and a folder's contents onto DEST. With
    /// several (`flux copy SOURCE... DEST`; the last path is DEST) each source, a file or
    /// a folder, is copied to `DEST/<its name>` in one operation; DEST is created if missing.
    /// --restart, --break-lock, --dry-run and --resume work with one or several sources.
    /// In a several-source run a file source that already exists at
    /// `DEST/<name>` is skipped without the identity check a single file gets, and when
    /// DEST's filesystem gives no strong file identity an existing file is reported as a
    /// collision.
    ///
    /// What happens to a file that already exists at DEST is chosen by one of
    /// --overwrite (the default: replace it), --update (replace it when the source is
    /// newer or the size differs; when either modification time is unavailable, only
    /// the sizes are compared, and equal sizes keep the file) or --skip-existing (leave it untouched); at most one
    /// may be given. A skipped file is not a failure. A symlink given as SOURCE is not
    /// followed.
    Copy(CopyArgs),
    /// Classify and remove what earlier operations left at DEST (§24.4, §251).
    Cleanup(CleanupArgs),
    /// Compare SOURCE with DEST by content, or several sources with their `DEST/<name>`, and report what differs;
    /// nothing is written.
    ///
    /// Every file is read in full on both sides and hashed. Exit 0: nothing missing, mismatched
    /// or unreadable; 1: something is; 2: a usage error, roots that are the same or nested, a refused source name,
    /// two sources that take one name or are one directory on the destination.
    #[command(after_long_help = verify_report::GUARANTEES)]
    Verify(VerifyArgs),
}

#[derive(Args)]
struct VerifyArgs {
    /// One or more sources, then DEST (the last path): each source is compared with DEST (§4.1's mapping); with several, with `DEST/<name>`.
    #[arg(value_name = "SOURCE... DEST", required = true, num_args = 2..)]
    paths: Vec<PathBuf>,
    /// `copy` (the default) compares a symlink's payload; `skip` leaves source symlinks out.
    #[arg(long, value_enum, default_value_t = LinksArg::Copy)]
    links: LinksArg,
    /// Also compare the contents of directories that are other mounts; by default each is reported as present and not entered.
    #[arg(long)]
    cross_filesystems: bool,
    /// The hash used to compare files.
    #[arg(long, value_enum, default_value_t = HashArg::Blake3)]
    hash: HashArg,
    /// One JSON object per record on stdout, then one {"summary": ...} object.
    #[arg(long)]
    json: bool,
    /// Also print each `match` record (the default prints only what is not a match).
    #[arg(long)]
    verbose: bool,
    /// Leave out every entry whose name or path matches PATTERN (repeatable; matched entries are neither compared nor
    /// reported, and not entered, on either side, and are counted as `excluded`). A pattern without `/` is matched against each entry's own name at any
    /// depth (`*.tmp`); with a leading or inner `/` it is anchored and matched against the whole path below the source
    /// root (`/build`, `docs/*.md`). `*` matches any run of characters within one name, `?` exactly one; a `**` segment
    /// matches whole path components (`a/**/b`; a trailing `**` matches everything inside). A trailing `/` limits the
    /// pattern to directories. Matching is case-sensitive. A pattern that starts with `-` is written
    /// `--exclude=-foo`. The characters `! [ ] \` are reserved and cannot be written: reach a name that has one with
    /// `?` or `*`. A source that is a file is not filtered, and a source operand itself (file or folder) is never matched; patterns apply below it.
    #[arg(long, value_name = "PATTERN", action = ArgAction::Append, value_parser = parse_exclude)]
    exclude: Vec<String>,
    /// Descend into subdirectories (the default; this flag only says so). Conflicts with --no-recursive.
    #[arg(long, conflicts_with = "no_recursive")]
    recursive: bool,
    /// Take only the top level of each source folder: only the top level of each side is compared; no subdirectory is entered.
    #[arg(long)]
    no_recursive: bool,
}

#[derive(Args)]
struct CleanupArgs {
    destination: PathBuf,
    /// Classify and report eligibility; delete nothing and take no lock.
    #[arg(long)]
    dry_run: bool,
    /// Also delete RESUMABLE operations (bypasses retention only; never a live or uncertain owner).
    #[arg(long)]
    force: bool,
    /// Print the report as one JSON object on stdout.
    #[arg(long)]
    json: bool,
}

#[derive(Args)]
#[command(group(clap::ArgGroup::new("existing").args(["overwrite", "update", "skip_existing"]).multiple(false)))]
struct CopyArgs {
    /// One or more sources, then DEST (the last path).
    #[arg(value_name = "SOURCE... DEST", required = true, num_args = 2..)]
    paths: Vec<PathBuf>,
    /// Fail a file if its timestamps cannot be applied (§44.1); a symlink's own times are not preserved.
    #[arg(long)]
    preserve_times: bool,
    /// Fail a file if its permissions cannot be applied (§44.1); a symlink's own permissions are not preserved.
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
    /// Continue the one resumable prior operation on DEST as itself: files it completed are verified and skipped,
    /// the rest are copied under the existing-file policy. With no prior operation a new one starts.
    /// In a tree copy a file of 64 MiB or more continues after the longest leading run of chunks whose digests match on
    /// both the source and the destination; every other half-copied file is recopied, and a half-copied single file of 64 MiB or more is validated by re-reading and hashing it against the source (`--resume-verify=chunks` runs as `full` for a single file). Under
    /// --durability=normal a checkpoint is advisory and is validated against the data before it is trusted.
    /// With several sources the per-root lines count this run's work only; files skipped as already complete are in the totals and the `resumed` line.
    #[arg(long, conflicts_with = "restart")]
    resume: bool,
    /// How a half-copied large file is validated before --resume continues it: `chunks` (the default) proves each
    /// recorded chunk's digest on both sides; `full` re-reads and hashes the whole partial against the source, needs no
    /// checkpoint log, and keeps the equal prefix; `metadata` is not implemented in this version.
    #[arg(long, value_enum, requires = "resume")]
    resume_verify: Option<ResumeVerifyArg>,
    /// With --restart: take over the DESTINATION lock that a crashed run left empty or unreadable (§240.5); a prefix
    /// lock is never taken over.
    #[arg(long, requires = "restart")]
    break_lock: bool,
    /// Replace a file that already exists at DEST (the default; §5.1).
    #[arg(long)]
    overwrite: bool,
    /// Replace an existing file at DEST when the source is newer or the size differs;
    /// with either modification time unavailable only the sizes are compared, and equal
    /// sizes keep the file (§5.1).
    #[arg(long)]
    update: bool,
    /// Leave an existing file at DEST untouched (§5.1).
    #[arg(long)]
    skip_existing: bool,
    /// How a symlink in the tree is copied: `copy` recreates it as a link with the same payload (§125); `skip`
    /// creates nothing and reports it as skipped (§124). `follow` is not supported.
    #[arg(long, value_enum, default_value_t = LinksArg::Copy)]
    links: LinksArg,
    /// How a file with several names in the tree is copied: `auto` (the default) copies it once and links the other
    /// selected names to it, copying independently and reporting where a link is impossible; `preserve` fails a member
    /// that cannot be linked (`HARDLINK_UNAVAILABLE`, `HARDLINK_IDENTITY_UNAVAILABLE`); `copy` copies every name
    /// separately (§15).
    #[arg(long, value_enum, default_value_t = HardlinksArg::Auto)]
    hardlinks: HardlinksArg,
    /// Descend into directories that are other mounts (section 42); by default each is created empty and skipped.
    #[arg(long)]
    cross_filesystems: bool,
    /// Report what the copy would do and change nothing: no lock, no state, no destination write (section 5.2). With several sources it cannot always tell that two names differing only in case are one name on a case-folding destination; the real run refuses that with DESTINATION_NAMESPACE_COLLISION.
    #[arg(long)]
    dry_run: bool,
    /// Check each copied file's content. `--verify` alone means `destination`. `none` (the default) checks nothing.
    /// `source-stream` hashes the source bytes while they are copied; nothing is compared in this version. `destination`
    /// reads the written file again and compares it with the source bytes that were read, so it detects a destination
    /// that returns different bytes than were written; the re-read may be served by the operating system's cache and does
    /// not prove the data reached stable storage (--durability still governs syncing). `full` is `destination` plus a
    /// second read of the source, which also detects source bytes that changed or were misread during the copy. A
    /// mismatch is reported as VERIFY_MISMATCH and counts as a failed file. With --resume the check covers the files this run copies or resumes; files a prior run completed are skipped, not verified by --verify.
    #[arg(long, value_enum, num_args = 0..=1, require_equals = true, default_missing_value = "destination")]
    verify: Option<VerifyArg>,
    /// The hash used by --verify.
    #[arg(long, value_enum, default_value_t = HashArg::Blake3)]
    hash: HashArg,
    /// Leave out every entry whose name or path matches PATTERN (repeatable; matched entries are neither copied nor
    /// entered, and are counted as `excluded`). A pattern without `/` is matched against each entry's own name at any
    /// depth (`*.tmp`); with a leading or inner `/` it is anchored and matched against the whole path below the source
    /// root (`/build`, `docs/*.md`). `*` matches any run of characters within one name, `?` exactly one; a `**` segment
    /// matches whole path components (`a/**/b`; a trailing `**` matches everything inside). A trailing `/` limits the
    /// pattern to directories. Matching is case-sensitive. A pattern that starts with `-` is written
    /// `--exclude=-foo`. The characters `! [ ] \` are reserved and cannot be written: reach a name that has one with
    /// `?` or `*`. A source that is a file is not filtered, and a source operand itself (file or folder) is never matched; patterns apply below it.
    #[arg(long, value_name = "PATTERN", action = ArgAction::Append, value_parser = parse_exclude)]
    exclude: Vec<String>,
    /// Descend into subdirectories (the default; this flag only says so). Conflicts with --no-recursive.
    #[arg(long, conflicts_with = "no_recursive")]
    recursive: bool,
    /// Take only the top level of each source folder: its files and links, no subdirectory is created or entered.
    #[arg(long)]
    no_recursive: bool,
}

#[derive(Clone, Copy, ValueEnum)]
enum VerifyArg {
    None,
    SourceStream,
    Destination,
    Full,
}

impl VerifyArg {
    fn level(self) -> flux_fs::VerifyLevel {
        match self {
            VerifyArg::None => flux_fs::VerifyLevel::None,
            VerifyArg::SourceStream => flux_fs::VerifyLevel::SourceStream,
            VerifyArg::Destination => flux_fs::VerifyLevel::Destination,
            VerifyArg::Full => flux_fs::VerifyLevel::Full,
        }
    }

    /// The `verify_level` string of the JSON report.
    fn name(self) -> &'static str {
        match self {
            VerifyArg::None => "none",
            VerifyArg::SourceStream => "source-stream",
            VerifyArg::Destination => "destination",
            VerifyArg::Full => "full",
        }
    }
}

#[derive(Clone, Copy, ValueEnum)]
enum HashArg {
    Blake3,
}

/// The requested level's report string; an absent `--verify` is `none`.
fn verify_name(args: &CopyArgs) -> &'static str {
    args.verify.map_or("none", VerifyArg::name)
}

#[derive(Clone, Copy, ValueEnum)]
enum DurabilityArg {
    Normal,
    Strict,
}

#[derive(Clone, Copy, ValueEnum)]
enum LinksArg {
    Copy,
    Skip,
}

#[derive(Clone, Copy, ValueEnum)]
enum HardlinksArg {
    Auto,
    Preserve,
    Copy,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum ResumeVerifyArg {
    Metadata,
    Chunks,
    Full,
}

#[derive(Clone, Copy, ValueEnum)]
enum SafetyArg {
    Default,
    Strict,
}

fn main() -> ExitCode {
    ExitCode::from(match Cli::parse().command {
        Commands::Copy(args) => copy(&args),
        Commands::Cleanup(args) => cleanup(&args),
        Commands::Verify(args) => verify(&args),
    })
}

/// Parses one `--exclude` value; the clap error is the pattern's own message.
fn parse_exclude(s: &str) -> Result<String, String> {
    flux_fs::Pattern::parse(s).map(|_| s.to_string()).map_err(|e| e.to_string())
}

/// The selection the flags ask for, with the aggregate caps checked.
fn selection_of(exclude: &[String], no_recursive: bool) -> Result<flux_fs::Selection, String> {
    flux_fs::Selection::compile(exclude, !no_recursive).map_err(|e| e.to_string())
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
        links: match args.links {
            LinksArg::Copy => Links::Copy,
            LinksArg::Skip => Links::Skip,
        },
        hardlinks: match args.hardlinks {
            HardlinksArg::Auto => flux_fs::Hardlinks::Auto,
            HardlinksArg::Preserve => flux_fs::Hardlinks::Preserve,
            HardlinksArg::Copy => flux_fs::Hardlinks::Copy,
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
        cross_filesystems: args.cross_filesystems,
        verify: args.verify.map_or(flux_fs::VerifyLevel::None, VerifyArg::level),
        hash: match args.hash {
            HashArg::Blake3 => flux_fs::HashAlgorithm::Blake3,
        },
        selection: selection_of(&args.exclude, args.no_recursive)
            .expect("validated at the top of copy/verify"),
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
        resume: args.resume,
        descriptor_limit: flux_platform::soft_descriptor_limit(),
        resume_verify: match args.resume_verify {
            None | Some(ResumeVerifyArg::Chunks) => flux_core::run::ResumeVerify::Chunks,
            Some(ResumeVerifyArg::Full) => flux_core::run::ResumeVerify::Full,
            // Refused first thing in `copy`.
            Some(ResumeVerifyArg::Metadata) => {
                unreachable!("--resume-verify=metadata is refused before the run")
            }
        },
        dry_run: args.dry_run,
        dry_temp_root: Some(std::env::temp_dir()),
        progress: None,
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

/// Cut 9e, known limit 6 (debug builds only): with `FLUX_TEST_COUNTERS_FILE=<path>`, a tree run writes `<path>` as one
/// line of JSON, `{"barrier_max_ms":<u64>,"cap_flushes":<u64>,"beat_gap_max_ms":<u64>}`: the longest data-sync barrier,
/// the flushes the open-writer cap forced, and the longest gap between two heartbeat writes. The acceptance measurement
/// reads it; nothing goes to stderr or `--json`. Every error is ignored: a test aid never fails a copy. A release build
/// ignores the variable.
#[cfg(debug_assertions)]
fn counters_file(outcome: &flux_core::tree::TreeOutcome, run_beat_gap_max: std::time::Duration) {
    let Some(path) = std::env::var_os("FLUX_TEST_COUNTERS_FILE") else {
        return;
    };
    let ms = |d: std::time::Duration| u64::try_from(d.as_millis()).unwrap_or(u64::MAX);
    let counters = serde_json::json!({
        "barrier_max_ms": ms(outcome.barrier_max),
        "cap_flushes": outcome.cap_flushes,
        "beat_gap_max_ms": ms(run_beat_gap_max),
    });
    let _ = std::fs::write(path, format!("{counters}\n"));
}

#[cfg(not(debug_assertions))]
fn counters_file(_outcome: &flux_core::tree::TreeOutcome, _run_beat_gap_max: std::time::Duration) {}

fn copy(args: &CopyArgs) -> u8 {
    if let Err(msg) = selection_of(&args.exclude, args.no_recursive) {
        err(&format!("error: {msg}"));
        return exit_code::USAGE;
    }
    if args.resume_verify == Some(ResumeVerifyArg::Metadata) {
        err(
            "error: --resume-verify=metadata is not implemented in this version; use chunks (the default) or full",
        );
        return exit_code::USAGE;
    }
    if args.paths.len() > 2 {
        return copy_several(args);
    }
    let (source, destination) = (&args.paths[0], &args.paths[1]);
    let job = match resolve::job(source, destination) {
        Ok(job) => job,
        Err(Stop::Usage(msg)) => {
            err(&format!("error: {msg}"));
            return exit_code::USAGE;
        }
        Err(Stop::SymlinkSource) => {
            err(&format!(
                "{}: {}: {}",
                Code::SymlinkCreationUnavailable.as_str(),
                source.display(),
                report::SYMLINK_WHY
            ));
            let mut r = Report::pre_engine(true);
            r.dry_run = args.dry_run;
            r.verify_level = verify_name(args);
            json(args, &r);
            return exit_code::FAILED;
        }
        Err(Stop::Failed(line)) => {
            err(&line);
            let mut r = Report::pre_engine(false);
            r.dry_run = args.dry_run;
            r.verify_level = verify_name(args);
            json(args, &r);
            return exit_code::FAILED;
        }
    };
    let opts = options(args);
    let mut cfg = run_config(args);
    let fs = flux_platform::StdFileSystem;
    let live = flux_cli::live::Live::start(
        flux_cli::live::display_wanted(args.dry_run, args.json),
        flux_ui::ProgressDrawTarget::stderr(),
        err,
    );
    cfg.progress = live.progress();
    match job {
        Job::Tree { src, dst } => {
            let started = Instant::now();
            let run = flux_core::run::tree(&fs, &src, &dst, &opts, &cfg, &mut |f| {
                let counted = flux_cli::live::counts_as_failure(&f.cause);
                for (index, line) in report::record_lines(&f).into_iter().enumerate() {
                    live.record(&line, counted && index == 0);
                }
            });
            live.finish();
            let ms = millis(started);
            let rep = Report::tree_run(&run, ms);
            tree_output(args, &opts, &run, rep, &[], None);
            exit_code::for_tree_run(&run)
        }
        Job::File { src, dst, target_existed } => {
            let started = Instant::now();
            let run = flux_core::run::file(&fs, &src, &dst, &opts, &cfg);
            live.finish();
            let ms = millis(started);
            let mut rep = Report::file_run(&run, target_existed, ms);
            rep.dry_run = args.dry_run;
            rep.verify_level = verify_name(args);
            if let Some(d) = &run.dry {
                rep.unprobed = report::unprobed_keys(d, args.resume);
            }
            if args.dry_run {
                rep.average_bytes_per_second = 0;
            }
            match &run.copy {
                Some(result) => {
                    match result {
                        Ok(o) => {
                            let target = dst.display().to_string();
                            if let Some(p) = run.dry.as_ref().and_then(|d| d.planned) {
                                err(&report::planned_file_line(&target, p));
                            }
                            if let Some(prior) = run.dry.as_ref().and_then(|d| d.prior.as_ref()) {
                                lines(report::dry_discard_lines(&dst, prior));
                            }
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
                    match &run.dry {
                        Some(d) => {
                            lines(d.prior.iter().flat_map(report::dry_prior_lines).collect())
                        }
                        None => lines(report::resume_lines(&run, 0)),
                    }
                    lines(report::file_checkpoint_lines(
                        &run,
                        &dst.display().to_string(),
                        opts.durability == Durability::Normal,
                    ));
                    lines(report::file_resume_verify_lines(&run));
                    let mode = if args.resume_verify == Some(ResumeVerifyArg::Full) {
                        "full"
                    } else {
                        "chunks"
                    };
                    for line in report::unprobed_lines(&rep.unprobed, mode) {
                        err(&line);
                    }
                    err(&if args.dry_run {
                        report::dry_run_summary_line(&rep, 0)
                    } else {
                        report::summary_line(&rep, 0)
                    });
                    if let Some(d) = &run.dry {
                        lines(report::dry_extra_lines(d, 0));
                    }
                }
                None => lines(report::run_lines(&run)),
            }
            json(args, &rep);
            exit_code::for_file_run(&run)
        }
    }
}

/// Everything a finished tree run prints, after its report is built: the stderr lines, the debug counters and the
/// `--json` object. `roots` is empty, and `several` (the destination) is `None`, for one source; a several-source run's roots print before the summary line.
fn tree_output(
    args: &CopyArgs,
    opts: &CopyOptions,
    run: &flux_core::run::Run<Result<flux_core::TreeOutcome, flux_core::TreeAbort>>,
    mut rep: Report,
    roots: &[flux_core::run::RootReport],
    several: Option<&std::path::Path>,
) {
    // The flag, not `run.dry`: an engine pre-flight refusal returns before the dry branch (ruling P7).
    rep.dry_run = args.dry_run;
    rep.verify_level = verify_name(args);
    if let Some(d) = &run.dry {
        rep.unprobed = report::unprobed_keys(d, args.resume);
    }
    if args.dry_run {
        // Nothing moved: the `timed()` throughput would be for bytes that never did.
        rep.average_bytes_per_second = 0;
    }
    let full = args.resume_verify == Some(ResumeVerifyArg::Full);
    let mode = if full { "full" } else { "chunks" };
    match &run.copy {
        Some(result) => {
            let outcome = match result {
                Ok(out) => out,
                Err(a) => {
                    err(&a.error.to_string());
                    lines(report::abort_hint_lines(&a.error, several));
                    &a.outcome
                }
            };
            for line in report::tree_warning_lines(outcome) {
                err(&line);
            }
            lines(report::run_lines(run));
            match &run.dry {
                Some(d) => lines(d.prior.iter().flat_map(report::dry_prior_lines).collect()),
                None => lines(report::resume_lines(run, outcome.files_resumed)),
            }
            lines(report::checkpoint_lines(outcome, opts.durability == Durability::Normal));
            lines(report::resume_verify_lines(outcome, full));
            rep.resume_verify = report::resume_verify_used(outcome, full).then_some("full");
            for line in report::unprobed_lines(&rep.unprobed, mode) {
                err(&line);
            }
            lines(report::root_lines(roots, args.dry_run));
            err(&if args.dry_run {
                report::dry_run_summary_line(&rep, outcome.directories_created)
            } else {
                report::summary_line(&rep, outcome.directories_created)
            });
            if let Some(d) = &run.dry {
                lines(report::dry_extra_lines(d, outcome.files_resumed));
            }
        }
        None => lines(report::run_lines(run)),
    }
    if let Some(result) = &run.copy {
        counters_file(
            match result {
                Ok(out) => out,
                Err(a) => &a.outcome,
            },
            run.beat_gap_max,
        );
    }
    json(args, &rep);
}

/// `flux copy SOURCE... DEST` with two or more sources (spec 3.1 to 3.8).
fn copy_several(args: &CopyArgs) -> u8 {
    if let Err(msg) = selection_of(&args.exclude, args.no_recursive) {
        err(&format!("error: {msg}"));
        return exit_code::USAGE;
    }
    let (sources, destination) = args.paths.split_at(args.paths.len() - 1);
    let mut cfg = run_config(args);
    let (specs, dst) = match resolve::several(
        sources,
        &destination[0],
        flux_core::run::source_cap(cfg.descriptor_limit),
        "copied onto",
    ) {
        Ok(planned) => planned,
        Err(Stop::Usage(msg)) => {
            err(&format!("error: {msg}"));
            return exit_code::USAGE;
        }
        Err(Stop::SymlinkSource) => {
            let named = resolve::first_symlink(sources).map(|p| p.display().to_string());
            err(&format!(
                "{}: {}: {}",
                Code::SymlinkCreationUnavailable.as_str(),
                named.unwrap_or_default(),
                report::SYMLINK_WHY
            ));
            let mut r = Report::pre_engine(true);
            r.dry_run = args.dry_run;
            r.verify_level = verify_name(args);
            json(args, &r);
            return exit_code::FAILED;
        }
        Err(Stop::Failed(line)) => {
            err(&line);
            let mut r = Report::pre_engine(false);
            r.dry_run = args.dry_run;
            r.verify_level = verify_name(args);
            json(args, &r);
            return exit_code::FAILED;
        }
    };
    let live = flux_cli::live::Live::start(
        flux_cli::live::display_wanted(args.dry_run, args.json),
        flux_ui::ProgressDrawTarget::stderr(),
        err,
    );
    cfg.progress = live.progress();
    let opts = options(args);
    let fs = flux_platform::StdFileSystem;
    let started = Instant::now();
    let multi = flux_core::run::trees(&fs, specs, &dst, &opts, &cfg, &mut |f| {
        let counted = flux_cli::live::counts_as_failure(&f.cause);
        for (index, line) in report::record_lines(&f).into_iter().enumerate() {
            live.record(&line, counted && index == 0);
        }
    });
    live.finish();
    let rep = Report::several_run(&multi, millis(started));
    tree_output(args, &opts, &multi.run, rep, &multi.roots, Some(&destination[0]));
    exit_code::for_multi_run(&multi)
}

/// The cleanup's configuration: retention and the lease are fixed (no flag, no environment variable), the ids are
/// fresh for this invocation.
fn cleanup_config(args: &CleanupArgs) -> flux_core::cleanup::CleanupConfig {
    flux_core::cleanup::CleanupConfig {
        retention: flux_core::cleanup::DEFAULT_RETENTION,
        lease_threshold: flux_core::cleanup::LEASE_THRESHOLD,
        now: flux_core::state::wall_time_ns(),
        force: args.force,
        dry_run: args.dry_run,
        operation_id: flux_core::ids::new_id(),
        owner_instance_id: flux_core::ids::new_id(),
        boot_session_id: flux_platform::boot_session_id(),
        before_mutation: debug_hook(),
        heartbeat_interval: heartbeat_interval(),
    }
}

/// Everything `flux cleanup` writes to stdout: with `--json` only the one-line object, else the table, the action
/// lines and the summary.
fn cleanup_stdout(
    args: &CleanupArgs,
    dst: &std::path::Path,
    rep: &flux_core::cleanup::CleanupReport,
) -> String {
    use flux_cli::cleanup_report as cr;
    if args.json {
        let j = cr::json(dst, args.dry_run, rep);
        return format!(
            "{}\n",
            serde_json::to_string(&j)
                .expect("plain structs of strings, numbers and bools serialize")
        );
    }
    let mut lines = vec![cr::header_line()];
    lines.extend(rep.entries.iter().map(cr::entry_line));
    lines.extend(rep.actions.iter().map(cr::action_line));
    lines.push(cr::summary_line(rep, args.dry_run));
    lines.join("\n") + "\n"
}

fn cleanup(args: &CleanupArgs) -> u8 {
    use flux_cli::cleanup_report as cr;
    let dst = match resolve::canonical_with_remainder(&args.destination) {
        Ok(p) => p,
        Err(e) => {
            err(&format!("DESTINATION_ERROR: {}: {e}", args.destination.display()));
            return exit_code::REFUSED;
        }
    };
    let cfg = cleanup_config(args);
    let rep = match flux_core::cleanup::cleanup(&flux_platform::StdFileSystem, &dst, &cfg) {
        Ok(rep) => rep,
        Err(refused) => {
            err(&cr::refused_line(&refused));
            return exit_code::REFUSED;
        }
    };
    let _ = write!(std::io::stdout(), "{}", cleanup_stdout(args, &dst, &rep));
    if let Some((path, e)) = &rep.listing_failed {
        err(&format!("error: could not list {}: {e}", path.display()));
    }
    if let Some((path, e)) = &rep.lost {
        err(&format!("error: the destination's lock was lost at {}: {e}", path.display()));
    }
    exit_code::for_cleanup(&rep)
}

/// The engine options the flags of `flux verify` select.
fn verify_options(args: &VerifyArgs) -> flux_core::verify::VerifyOptions {
    use flux_core::walk::MountPolicy;
    flux_core::verify::VerifyOptions {
        links: match args.links {
            LinksArg::Copy => Links::Copy,
            LinksArg::Skip => Links::Skip,
        },
        mounts: if args.cross_filesystems { MountPolicy::Cross } else { MountPolicy::Stay },
        hash: match args.hash {
            HashArg::Blake3 => flux_fs::HashAlgorithm::Blake3,
        },
        selection: selection_of(&args.exclude, args.no_recursive)
            .expect("validated at the top of copy/verify"),
    }
}

/// `flux verify` (cut 14): design 6.1 and 6.6. Records go to stdout; stderr carries only operational lines.
fn verify(args: &VerifyArgs) -> u8 {
    if let Err(msg) = selection_of(&args.exclude, args.no_recursive) {
        err(&format!("error: {msg}"));
        return exit_code::USAGE;
    }
    if args.paths.len() == 2 {
        verify_one(args, &args.paths[0], &args.paths[1])
    } else {
        verify_several_cli(args)
    }
}

/// `flux verify SOURCE DEST`.
fn verify_one(args: &VerifyArgs, source: &Path, dest: &Path) -> u8 {
    // `--json` still prints a summary when the run stops before any walk with exit 1 (plan R7).
    let stopped = || {
        if args.json {
            let s = flux_core::verify::Summary::default();
            stdout_or_exit(&verify_report::summary_json(
                verify_report::Sources::One(source),
                dest,
                "blake3",
                &s,
                exit_code::FAILED,
            ));
        }
        exit_code::FAILED
    };
    let job = match resolve::job(source, dest) {
        Ok(job) => job,
        Err(Stop::Usage(msg)) => {
            err(&format!("error: {msg}"));
            return exit_code::USAGE;
        }
        Err(Stop::SymlinkSource) => {
            err(&format!(
                "{}: {}: {}",
                Code::SymlinkCreationUnavailable.as_str(),
                source.display(),
                report::SYMLINK_WHY
            ));
            return stopped();
        }
        Err(Stop::Failed(line)) => {
            err(&line);
            return stopped();
        }
    };
    let opts = verify_options(args);
    let mut sink = |r: flux_core::verify::Record| {
        let routed = verify_report::route_record(&r, args.json, args.verbose);
        if let Some(line) = &routed.stdout {
            stdout_or_exit(line);
        }
        lines(routed.stderr);
    };
    let fs = flux_platform::StdFileSystem;
    let (src, dst, result) = match job {
        Job::Tree { src, dst } => {
            // The engine says so after the root check passed, so a refused run prints no such line.
            let mut on_absent = || err(&verify_report::dest_absent_line(&dst));
            let result = flux_core::verify::verify_tree_notifying(
                &fs,
                &src,
                &dst,
                &opts,
                &mut on_absent,
                &mut sink,
            );
            (src, dst, result)
        }
        Job::File { src, dst, .. } => {
            let result = flux_core::verify::verify_file(&fs, &src, &dst, &opts, &mut sink);
            (src, dst, result)
        }
    };
    let summary = match result {
        Ok(summary) => summary,
        Err(e) => {
            err(&refusal_line(&e, &dst));
            return exit_code::USAGE;
        }
    };
    let code = exit_code::for_verify(&summary);
    let line = if args.json {
        verify_report::summary_json(
            verify_report::Sources::One(&src),
            &dst,
            "blake3",
            &summary,
            code,
        )
    } else {
        verify_report::summary_line(&summary)
    };
    stdout_or_exit(&line);
    code
}

/// `flux verify SOURCE... DEST` with two or more sources (design 6.1, 6.5, 6.6): each root against `DEST/<name>`.
fn verify_several_cli(args: &VerifyArgs) -> u8 {
    let (sources, destination) = args.paths.split_at(args.paths.len() - 1);
    let destination = &destination[0];
    let stopped = || {
        if args.json {
            stdout_or_exit(&verify_report::summary_json(
                verify_report::Sources::Several(sources),
                destination,
                "blake3",
                &flux_core::verify::Summary::default(),
                exit_code::FAILED,
            ));
        }
        exit_code::FAILED
    };
    let (specs, dst) = match resolve::several(sources, destination, usize::MAX, "verified against")
    {
        Ok(planned) => planned,
        Err(Stop::Usage(msg)) => {
            err(&format!("error: {msg}"));
            return exit_code::USAGE;
        }
        Err(Stop::SymlinkSource) => {
            let named = resolve::first_symlink(sources).map(|p| p.display().to_string());
            err(&format!(
                "{}: {}: {}",
                Code::SymlinkCreationUnavailable.as_str(),
                named.unwrap_or_default(),
                report::SYMLINK_WHY
            ));
            return stopped();
        }
        Err(Stop::Failed(line)) => {
            err(&line);
            return stopped();
        }
    };
    // `verify_several` takes the specs by value and reorders them; the summary lists them as typed.
    let collected: Vec<PathBuf> = specs.iter().map(|s| s.src.clone()).collect();
    let opts = verify_options(args);
    let mut sink = |r: flux_core::verify::Record| {
        let routed = verify_report::route_record(&r, args.json, args.verbose);
        if let Some(line) = &routed.stdout {
            stdout_or_exit(line);
        }
        lines(routed.stderr);
    };
    let mut on_absent = |name: &std::ffi::OsStr| err(&verify_report::root_absent_line(&dst, name));
    let mut on_root_done = |name: &std::ffi::OsStr, s: &flux_core::verify::Summary| {
        err(&verify_report::root_line(name, s))
    };
    let fs = flux_platform::StdFileSystem;
    let total = match flux_core::verify::verify_several(
        &fs,
        specs,
        &dst,
        &opts,
        &mut on_absent,
        &mut on_root_done,
        &mut sink,
    ) {
        Ok(total) => total,
        Err(e) => {
            err(&refusal_line(&e, &dst));
            return exit_code::USAGE;
        }
    };
    let code = exit_code::for_verify(&total);
    let line = if args.json {
        verify_report::summary_json(
            verify_report::Sources::Several(&collected),
            &dst,
            "blake3",
            &total,
            code,
        )
    } else {
        verify_report::summary_line(&total)
    };
    stdout_or_exit(&line);
    code
}

/// The one error line for a verify that did not run (exit 2); `dst` is the canonical destination.
fn refusal_line(e: &VerifyError, dst: &Path) -> String {
    match e {
        VerifyError::RefusedRoots { reason } | VerifyError::RefusedNames { reason } => {
            format!("error: {reason}")
        }
        VerifyError::RefusedRoot { name, reason } => {
            verify_report::refused_root_line(name, dst, reason)
        }
        VerifyError::RefusedAlias { a, b } => verify_report::refused_alias_line(a, b),
    }
}

/// One line of `flux verify`'s output to stdout. A failed write (a closed pipe, a full disk) ends the run at once with
/// exit 1: verify is read-only and holds no lock, and the verdict is unknown once its output is lost, so continuing to
/// hash the tree would only waste the work. A broken pipe is the reader's choice and says nothing.
fn stdout_or_exit(line: &str) {
    if let Err(e) = writeln!(std::io::stdout(), "{line}") {
        if e.kind() != std::io::ErrorKind::BrokenPipe {
            err(&format!("error: cannot write to standard output: {e}"));
        }
        std::process::exit(i32::from(exit_code::FAILED));
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
        let Commands::Copy(args) = Cli::try_parse_from(argv).unwrap().command else {
            panic!("not a copy");
        };
        args
    }

    fn parse_verify(argv: &[&str]) -> VerifyArgs {
        let Commands::Verify(args) = Cli::try_parse_from(argv).unwrap().command else {
            panic!("not a verify");
        };
        args
    }

    #[test]
    fn cross_filesystems_reaches_the_engine_mount_policy() {
        use flux_core::walk::MountPolicy;
        let stay = verify_options(&parse_verify(&["flux", "verify", "s", "d"]));
        assert_eq!(stay.mounts, MountPolicy::Stay);
        let cross =
            verify_options(&parse_verify(&["flux", "verify", "s", "d", "--cross-filesystems"]));
        assert_eq!(cross.mounts, MountPolicy::Cross);
        let skip = verify_options(&parse_verify(&["flux", "verify", "s", "d", "--links", "skip"]));
        assert!(matches!(skip.links, Links::Skip));
    }

    #[test]
    fn verify_parses_its_flags_and_needs_at_least_two_paths() {
        let bare = parse_verify(&["flux", "verify", "s", "d"]);
        assert_eq!(bare.paths[0], PathBuf::from("s"));
        assert_eq!(bare.paths[1], PathBuf::from("d"));
        assert!(matches!(bare.links, LinksArg::Copy));
        assert!(!bare.cross_filesystems && !bare.json && !bare.verbose);
        let all = parse_verify(&[
            "flux",
            "verify",
            "s",
            "d",
            "--links",
            "skip",
            "--cross-filesystems",
            "--json",
            "--verbose",
        ]);
        assert!(matches!(all.links, LinksArg::Skip));
        assert!(all.cross_filesystems && all.json && all.verbose);
        for argv in [&["flux", "verify", "s"][..], &["flux", "verify", "s", "d", "--resume"]] {
            assert!(Cli::try_parse_from(argv).is_err(), "{argv:?}");
        }
        assert_eq!(parse_verify(&["flux", "verify", "s", "d", "--exclude", "x"]).exclude, ["x"]);
        assert_eq!(parse_verify(&["flux", "verify", "s", "t", "d"]).paths.len(), 3);
    }

    #[test]
    fn exclude_is_repeatable_and_parses() {
        let c = parse(&["--exclude", "*.tmp", "--exclude=-foo"]);
        assert_eq!(c.exclude, ["*.tmp", "-foo"]);
        assert!(!c.recursive && !c.no_recursive);
        let v = parse_verify(&["flux", "verify", "s", "d", "--exclude", "a", "--exclude", "b/c"]);
        assert_eq!(v.exclude, ["a", "b/c"]);
        assert!(parse(&["--recursive"]).recursive);
        assert!(parse_verify(&["flux", "verify", "s", "d", "--no-recursive"]).no_recursive);
    }

    #[test]
    fn recursive_and_no_recursive_conflict() {
        for argv in [
            &["flux", "copy", "s", "d", "--recursive", "--no-recursive"][..],
            &["flux", "verify", "s", "d", "--no-recursive", "--recursive"],
        ] {
            assert!(Cli::try_parse_from(argv).is_err(), "{argv:?}");
        }
    }

    #[test]
    fn a_bad_pattern_is_a_parse_error() {
        for argv in [
            &["flux", "copy", "s", "d", "--exclude", "a//b"][..],
            &["flux", "verify", "s", "d", "--exclude", ""],
        ] {
            assert!(Cli::try_parse_from(argv).is_err(), "{argv:?}");
        }
        assert!(selection_of(&["a\\b".to_string()], false).is_err());
        assert!(selection_of(&["*.tmp".to_string()], true).is_ok());
    }

    fn parse_cleanup(argv: &[&str]) -> CleanupArgs {
        let Commands::Cleanup(args) = Cli::try_parse_from(argv).unwrap().command else {
            panic!("not a cleanup");
        };
        args
    }

    #[test]
    fn cleanup_parses_its_three_flags() {
        let bare = parse_cleanup(&["flux", "cleanup", "d"]);
        assert_eq!(bare.destination, PathBuf::from("d"));
        assert!(!bare.dry_run && !bare.force && !bare.json);
        let all = parse_cleanup(&["flux", "cleanup", "d", "--dry-run", "--force", "--json"]);
        assert!(all.dry_run && all.force && all.json);
        let cfg = cleanup_config(&all);
        assert!(cfg.dry_run && cfg.force);
        assert_eq!(cfg.retention, flux_core::cleanup::DEFAULT_RETENTION);
        assert_eq!(cfg.lease_threshold, flux_core::cleanup::LEASE_THRESHOLD);
        assert_ne!(cfg.operation_id, cfg.owner_instance_id);
    }

    #[test]
    fn json_stdout_is_one_object_and_text_stdout_is_the_table() {
        let rep = flux_core::cleanup::CleanupReport::default();
        let p = std::path::Path::new("/d");
        let json = cleanup_stdout(&parse_cleanup(&["flux", "cleanup", "d", "--json"]), p, &rep);
        assert_eq!(json.lines().count(), 1);
        assert!(json.starts_with("{\"destination\":\"/d\",\"dry_run\":false,"), "{json}");
        let text = cleanup_stdout(&parse_cleanup(&["flux", "cleanup", "d"]), p, &rep);
        assert!(
            text.starts_with("STATUS ")
                && text.ends_with("cleanup: 0 entries, 0 removed, 0 kept, 0 skipped\n")
        );
        assert!(!text.contains('{'));
    }

    #[test]
    fn a_bare_cleanup_is_a_usage_error() {
        let err = Cli::try_parse_from(["flux", "cleanup"]).err().expect("DEST is required");
        assert_eq!(err.exit_code(), 2);
        assert!(Cli::try_parse_from(["flux", "cleanup", "d", "--bogus"]).is_err());
    }

    #[test]
    fn an_unresolvable_destination_exits_3_not_1() {
        // A NUL byte is not NotFound, so canonicalisation returns the error instead of climbing.
        let args = CleanupArgs {
            destination: PathBuf::from("a\0b"),
            dry_run: true,
            force: false,
            json: false,
        };
        assert_eq!(cleanup(&args), exit_code::REFUSED);
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
    fn hardlinks_defaults_to_auto_and_each_mode_reaches_the_options() {
        use flux_fs::Hardlinks;
        assert_eq!(options(&parse(&[])).hardlinks, Hardlinks::Auto);
        assert_eq!(options(&parse(&["--hardlinks=preserve"])).hardlinks, Hardlinks::Preserve);
        assert_eq!(options(&parse(&["--hardlinks=copy"])).hardlinks, Hardlinks::Copy);
        assert_eq!(options(&parse(&["--hardlinks=auto"])).hardlinks, Hardlinks::Auto);
    }

    #[test]
    fn safety_strict_reaches_the_options() {
        assert_eq!(options(&parse(&["--safety=strict"])).safety, Safety::Strict);
    }

    #[test]
    fn dry_run_reaches_run_config_and_never_the_options() {
        assert!(!run_config(&parse(&[])).dry_run);
        assert!(run_config(&parse(&["--dry-run"])).dry_run);
        let (with, without) = (options(&parse(&["--dry-run"])), options(&parse(&[])));
        assert_eq!(format!("{with:?}"), format!("{without:?}"));
    }

    #[test]
    fn cross_filesystems_reaches_the_options() {
        assert!(!options(&parse(&[])).cross_filesystems);
        assert!(options(&parse(&["--cross-filesystems"])).cross_filesystems);
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
    fn verify_flag_forms() {
        use flux_fs::VerifyLevel;
        assert_eq!(options(&parse(&[])).verify, VerifyLevel::None);
        assert_eq!(options(&parse(&["--verify"])).verify, VerifyLevel::Destination);
        assert_eq!(options(&parse(&["--verify=full"])).verify, VerifyLevel::Full);
        assert_eq!(options(&parse(&["--verify=none"])).verify, VerifyLevel::None);
        assert_eq!(options(&parse(&["--verify=source-stream"])).verify, VerifyLevel::SourceStream);
        assert_eq!(options(&parse(&["--verify=destination"])).verify, VerifyLevel::Destination);
    }

    #[test]
    fn a_bare_verify_before_the_positionals_does_not_eat_a_path() {
        use flux_fs::VerifyLevel;
        let args = Cli::try_parse_from(["flux", "copy", "--verify", "a", "b"]).unwrap();
        let Commands::Copy(args) = args.command else { panic!("not a copy") };
        assert_eq!(args.paths, [PathBuf::from("a"), PathBuf::from("b")]);
        assert_eq!(options(&args).verify, VerifyLevel::Destination);
    }

    #[test]
    fn an_unknown_level_and_an_unknown_hash_are_usage_errors() {
        for bad in ["--verify=bogus", "--hash=sha256"] {
            let err = Cli::try_parse_from(["flux", "copy", "a", "b", bad])
                .err()
                .expect("must be rejected");
            assert_eq!(err.exit_code(), 2, "{bad}");
        }
    }

    #[test]
    fn hash_defaults_to_blake3() {
        assert_eq!(options(&parse(&[])).hash, flux_fs::HashAlgorithm::Blake3);
        assert_eq!(options(&parse(&["--hash=blake3"])).hash, flux_fs::HashAlgorithm::Blake3);
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
    fn resume_and_restart_together_is_a_usage_error() {
        let err = Cli::try_parse_from(["flux", "copy", "a", "b", "--resume", "--restart"])
            .err()
            .expect("--resume with --restart must be rejected");
        assert_eq!(err.exit_code(), 2);
    }

    #[test]
    fn resume_reaches_the_config() {
        assert!(run_config(&parse(&["--resume"])).resume);
        assert!(!run_config(&parse(&[])).resume);
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
