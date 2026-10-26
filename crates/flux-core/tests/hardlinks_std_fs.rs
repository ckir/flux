// Copyright (c) 2026 Costas Kirgoussios
//
// This software is licensed under the PolyForm Noncommercial License 1.0.0.
// You may obtain a copy of the License at:
// https://polyformproject.org/licenses/noncommercial/1.0.0/

//! Cut 12: hardlink preservation against a REAL filesystem, on every CI leg (master section 69, scaled to CI). The
//! engine's rules are pinned against `FaultFs` in `src/tree.rs`; these tests drive `run::tree` with
//! `StdFileSystem`, so the claim store, the workspace and the redb hardlink index are real too.
//!
//! Link counts and identities are read through `FileSystem::metadata`, never through `MetadataExt`, so the same
//! assertions hold on Windows and macOS. A filesystem that cannot report a link count fails these tests loudly: they
//! are first-class, not skippable.

use flux_core::run::{self, RunConfig};
use flux_core::{TreeFailure, TreeOutcome};
use flux_fs::{
    CopyOptions, Durability, ExistingPolicy, FileIdentity, FileSystem, Hardlinks, Links,
    OperationId, Preserve, Publish, Safety,
};
use flux_platform::StdFileSystem;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tempfile::TempDir;

fn opts(hardlinks: Hardlinks, existing: ExistingPolicy) -> CopyOptions {
    CopyOptions {
        preserve_times: Preserve::Default,
        preserve_permissions: Preserve::Default,
        durability: Durability::Normal,
        publish: Publish::Replace,
        safety: Safety::Default,
        operation_id: OperationId::new("t"),
        existing,
        links: Links::Copy,
        hardlinks,
        cross_filesystems: false,
    }
}

fn auto() -> CopyOptions {
    opts(Hardlinks::Auto, ExistingPolicy::Overwrite)
}

fn config() -> RunConfig {
    RunConfig {
        restart: false,
        break_lock: false,
        operation_id: flux_core::ids::new_id(),
        owner_instance_id: flux_core::ids::new_id(),
        boot_session_id: flux_platform::boot_session_id(),
        before_mutation: None,
        heartbeat_interval: run::HEARTBEAT_INTERVAL,
        resume: false,
        descriptor_limit: None,
        resume_verify: run::ResumeVerify::Chunks,
        dry_run: false,
        dry_temp_root: None,
    }
}

/// One real tree run; the walk must complete and the run must not stop early.
fn run_tree(
    src: &Path,
    dst: &Path,
    o: &CopyOptions,
    cfg: &RunConfig,
) -> (TreeOutcome, Vec<TreeFailure>) {
    let mut failures = Vec::new();
    let r = run::tree(&StdFileSystem, src, dst, o, cfg, &mut |f| failures.push(f));
    assert!(r.stop.is_none(), "the run stopped: {:?}", r.stop);
    match r.copy {
        Some(Ok(out)) => (out, failures),
        Some(Err(a)) => panic!("the walk aborted: {:?}", a.error),
        None => panic!("no copy outcome"),
    }
}

/// A clean run: no failure record of any kind.
fn run_clean(src: &Path, dst: &Path, o: &CopyOptions) -> TreeOutcome {
    let (out, failures) = run_tree(src, dst, o, &config());
    assert!(failures.is_empty(), "{failures:?}");
    out
}

fn nlink(p: &Path) -> Option<u64> {
    StdFileSystem.metadata(p).unwrap().nlink
}

/// The object a name leads to, which must be a `Strong` identity on every CI filesystem.
fn object(p: &Path) -> flux_fs::ObjectId {
    match StdFileSystem.metadata(p).unwrap().identity {
        FileIdentity::Strong(id) => id,
        other => panic!("{}: identity is not strong: {other:?}", p.display()),
    }
}

fn same_object(a: &Path, b: &Path) -> bool {
    object(a) == object(b)
}

/// A source directory and a destination beside it.
fn fixture() -> (TempDir, PathBuf, PathBuf) {
    let d = TempDir::new().unwrap();
    let (src, dst) = (d.path().join("src"), d.path().join("dst"));
    std::fs::create_dir(&src).unwrap();
    (d, src, dst)
}

#[test]
fn one_pair() {
    let (_d, src, dst) = fixture();
    std::fs::write(src.join("f"), b"payload").unwrap();
    std::fs::hard_link(src.join("f"), src.join("g")).unwrap();
    assert_eq!(nlink(&src.join("f")), Some(2), "the source fixture is a real pair");

    let out = run_clean(&src, &dst, &auto());

    assert_eq!(out.files_hardlinked, 1);
    assert_eq!(out.hardlinks_degraded, 0);
    assert_eq!((out.files_copied, out.bytes_copied), (1, 7));
    assert!(same_object(&dst.join("f"), &dst.join("g")));
    assert_eq!(nlink(&dst.join("f")), Some(2));
    assert_eq!(std::fs::read(dst.join("g")).unwrap(), b"payload");
}

#[test]
fn ten_members() {
    let (_d, src, dst) = fixture();
    let bytes = vec![b'x'; 12_345];
    std::fs::write(src.join("m0"), &bytes).unwrap();
    for i in 1..10 {
        std::fs::hard_link(src.join("m0"), src.join(format!("m{i}"))).unwrap();
    }
    assert_eq!(nlink(&src.join("m0")), Some(10));

    let out = run_clean(&src, &dst, &auto());

    assert_eq!(out.files_hardlinked, 9);
    assert_eq!(out.hardlinks_degraded, 0);
    assert_eq!(out.files_copied, 1, "the group is one transfer");
    assert_eq!(out.bytes_copied, bytes.len() as u64);
    assert_eq!(nlink(&dst.join("m0")), Some(10));
    for i in 1..10 {
        assert!(same_object(&dst.join("m0"), &dst.join(format!("m{i}"))), "m{i}");
    }
}

/// Measured with `cargo nextest run -p flux-core --test hardlinks_std_fs` on the new dev box (Linux ext4, 8 cores): 9.6 s
/// (a second, unrelated run of the same binary took 9.8 s). An uncontrolled single measurement, taken with the machine's
/// background load not controlled; a figure of this machine, not a budget.
#[test]
fn a_huge_tree_of_two_thousand_pairs_links_every_pair() {
    const PAIRS: u64 = 2000;
    let (_d, src, dst) = fixture();
    for i in 0..PAIRS {
        let dir = src.join(format!("d{i:04}"));
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("f"), format!("content {i}")).unwrap();
        std::fs::hard_link(dir.join("f"), dir.join("g")).unwrap();
    }

    let out = run_clean(&src, &dst, &auto());

    assert_eq!(out.files_hardlinked, PAIRS);
    assert_eq!(out.files_copied, PAIRS, "one transfer per pair");
    assert_eq!(out.hardlinks_degraded, 0);
    for i in 0..PAIRS {
        let dir = dst.join(format!("d{i:04}"));
        assert!(same_object(&dir.join("f"), &dir.join("g")), "d{i:04}");
        assert_eq!(nlink(&dir.join("f")), Some(2), "d{i:04}");
    }
}

#[test]
fn a_group_across_directories_is_one_object_on_a_real_filesystem() {
    let (_d, src, dst) = fixture();
    std::fs::create_dir(src.join("z")).unwrap();
    std::fs::create_dir(src.join("a")).unwrap();
    std::fs::write(src.join("z").join("f"), b"shared").unwrap();
    std::fs::hard_link(src.join("z").join("f"), src.join("a").join("f")).unwrap();

    let out = run_clean(&src, &dst, &auto());

    assert_eq!(out.files_copied, 1, "one transfer for the group");
    assert_eq!(out.bytes_copied, 6);
    assert_eq!(out.files_hardlinked, 1);
    assert!(same_object(&dst.join("z").join("f"), &dst.join("a").join("f")));
    assert_eq!(nlink(&dst.join("a").join("f")), Some(2));
}

#[test]
fn copy_mode_copies_every_name_on_a_real_filesystem() {
    let (_d, src, dst) = fixture();
    std::fs::write(src.join("f"), b"payload").unwrap();
    std::fs::hard_link(src.join("f"), src.join("g")).unwrap();

    let out = run_clean(&src, &dst, &opts(Hardlinks::Copy, ExistingPolicy::Overwrite));

    assert_eq!(out.files_hardlinked, 0);
    assert_eq!(out.files_copied, 2);
    assert_eq!(out.bytes_copied, 14);
    assert!(!same_object(&dst.join("f"), &dst.join("g")), "two distinct objects");
    assert_eq!(nlink(&dst.join("f")), Some(1));
    assert_eq!(nlink(&dst.join("g")), Some(1));
}

#[test]
fn a_second_run_over_an_already_linked_destination_under_skip_existing_reports_no_degradations() {
    let (_d, src, dst) = fixture();
    std::fs::write(src.join("f"), b"payload").unwrap();
    std::fs::hard_link(src.join("f"), src.join("g")).unwrap();
    let first = run_clean(&src, &dst, &auto());
    assert_eq!(first.files_hardlinked, 1);

    let second = run_clean(&src, &dst, &opts(Hardlinks::Auto, ExistingPolicy::SkipExisting));

    assert_eq!(second.files_skipped, 2, "both names are skipped");
    assert_eq!(second.hardlinks_degraded, 0);
    assert_eq!(second.files_hardlinked, 0);
    assert_eq!(second.files_copied, 0);
    assert!(same_object(&dst.join("f"), &dst.join("g")), "the link is left as it was");
}

#[cfg(unix)]
#[test]
fn the_symlink_sibling_of_a_group_is_untouched() {
    let (_d, src, dst) = fixture();
    std::fs::write(src.join("f"), b"payload").unwrap();
    std::fs::hard_link(src.join("f"), src.join("g")).unwrap();
    std::os::unix::fs::symlink("f", src.join("s")).unwrap();
    // `Links::Copy` recreates a symlink as a symlink.
    let out = run_clean(&src, &dst, &auto());

    assert_eq!(out.files_hardlinked, 1);
    assert_eq!(out.hardlinks_degraded, 0);
    assert!(same_object(&dst.join("f"), &dst.join("g")));
    assert_eq!(nlink(&dst.join("f")), Some(2), "the symlink is not a third name of the object");
    let md = std::fs::symlink_metadata(dst.join("s")).unwrap();
    assert!(md.file_type().is_symlink(), "the symlink stays a symlink");
    assert_eq!(std::fs::read_link(dst.join("s")).unwrap(), Path::new("f"));
}

/// Builds `src/f` and `src/g` as a pair in a scratch directory and runs it, cutting the run short by panic at the
/// `cut`th guarded mutation (`None` runs to the end). Returns the fixture and the number of mutations the hook saw.
fn pair_run(cut: Option<u64>) -> (TempDir, PathBuf, PathBuf, u64, bool) {
    let (d, src, dst) = fixture();
    std::fs::write(src.join("f"), b"payload").unwrap();
    std::fs::hard_link(src.join("f"), src.join("g")).unwrap();
    std::fs::create_dir(&dst).unwrap();
    let seen = Arc::new(AtomicU64::new(0));
    let counter = Arc::clone(&seen);
    let cfg = RunConfig {
        before_mutation: Some(Arc::new(move || {
            if counter.fetch_add(1, Ordering::SeqCst) + 1 == cut.unwrap_or(u64::MAX) {
                panic!("cut short on purpose");
            }
        })),
        ..config()
    };
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run::tree(&StdFileSystem, &src, &dst, &auto(), &cfg, &mut |_| {})
    }));
    let panicked = r.is_err();
    (d, src, dst, seen.load(Ordering::SeqCst), panicked)
}

#[test]
fn a_resume_after_an_interrupted_run_links_the_rest() {
    // Count the guarded mutations of an uninterrupted reference run in a scratch directory.
    let (_ref_dir, _, ref_dst, total, panicked) = pair_run(None);
    assert!(!panicked);
    assert!(total >= 2, "the reference run mutated {total} times");
    assert!(ref_dst.join("f").exists() && ref_dst.join("g").exists());

    // The cut that leaves exactly the canonical published and the dependent absent: the first mutation after
    // which one name exists and the other does not. Found by measurement on the same fixture, then pinned below.
    let cut = (1..=total)
        .find(|&c| {
            let (_d, _, dst, _, panicked) = pair_run(Some(c));
            panicked && dst.join("f").exists() != dst.join("g").exists()
        })
        .expect("some cut leaves only the canonical");

    let (_d, src, dst, _, panicked) = pair_run(Some(cut));
    assert!(panicked, "the run was cut short");
    let (canonical, dependent) = if dst.join("f").exists() { ("f", "g") } else { ("g", "f") };
    assert!(dst.join(canonical).is_file(), "the canonical is published");
    assert!(!dst.join(dependent).exists(), "the dependent is absent");
    assert_eq!(std::fs::read(dst.join(canonical)).unwrap(), b"payload");

    let cfg = RunConfig { resume: true, ..config() };
    let (out, failures) = run_tree(&src, &dst, &auto(), &cfg);

    assert!(failures.is_empty(), "{failures:?}");
    assert_eq!(out.files_hardlinked, 1);
    assert_eq!(out.hardlinks_degraded, 0);
    assert!(same_object(&dst.join("f"), &dst.join("g")), "one object behind both names");
    assert_eq!(nlink(&dst.join("f")), Some(2));
    assert_eq!(std::fs::read(dst.join(dependent)).unwrap(), b"payload");
}
