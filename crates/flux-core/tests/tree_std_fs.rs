//! `copy_tree` against real filesystems, on every CI leg. The engine's rules are pinned
//! against `FaultFs` in `src/tree.rs`; this proves a real filesystem reports what they
//! rely on - identity, `AlreadyExists`, and a followed destination link.

use flux_core::copy::CopyError;
use flux_core::{TreeFailure, TreeFailureCause, TreeOutcome, copy_tree};
use flux_fs::{Code, CopyOptions, Durability, OperationId, Preserve, Publish, Safety};
use flux_platform::StdFileSystem;
use std::path::Path;
use tempfile::TempDir;

fn opts() -> CopyOptions {
    CopyOptions {
        preserve_times: Preserve::Default,
        preserve_permissions: Preserve::Default,
        durability: Durability::Normal,
        publish: Publish::Replace,
        safety: Safety::Default,
        operation_id: OperationId::new("t"),
    }
}

fn run(src: &Path, dst: &Path) -> (Result<TreeOutcome, CopyError>, Vec<TreeFailure>) {
    let mut got = Vec::new();
    let r = copy_tree(&StdFileSystem, src, dst, &opts(), &mut |f| got.push(f)).map_err(|a| a.error);
    (r, got)
}

fn names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

#[test]
fn a_real_tree_is_copied_into_a_fresh_destination() {
    let d = TempDir::new().unwrap();
    let (src, dst) = (d.path().join("src"), d.path().join("dst"));
    std::fs::create_dir_all(src.join("sub")).unwrap();
    std::fs::write(src.join("a.txt"), b"A").unwrap();
    std::fs::write(src.join("sub").join("b.txt"), b"BB").unwrap();

    let (r, got) = run(&src, &dst);

    let out = r.unwrap();
    assert!(got.is_empty(), "{got:?}");
    assert_eq!((out.files_copied, out.bytes_copied, out.directories_created), (2, 3, 2));
    assert_eq!(std::fs::read(dst.join("a.txt")).unwrap(), b"A");
    assert_eq!(std::fs::read(dst.join("sub").join("b.txt")).unwrap(), b"BB");
    assert_eq!(names(&dst), ["a.txt", "sub"], "no temporary left behind");
}

#[test]
fn an_existing_destination_file_is_left_intact_and_reported() {
    let d = TempDir::new().unwrap();
    let (src, dst) = (d.path().join("src"), d.path().join("dst"));
    std::fs::create_dir(&src).unwrap();
    std::fs::create_dir(&dst).unwrap();
    std::fs::write(src.join("a.txt"), b"new").unwrap();
    std::fs::write(dst.join("a.txt"), b"old").unwrap();

    let (r, got) = run(&src, &dst);

    r.unwrap();
    assert_eq!(got.len(), 1, "{got:?}");
    assert_eq!(got[0].path, Path::new("a.txt"));
    match &got[0].cause {
        TreeFailureCause::Copy(e) => assert_eq!(e.code(), Code::DestinationNamespaceCollision),
        other => panic!("expected Copy, got {other:?}"),
    }
    assert_eq!(std::fs::read(dst.join("a.txt")).unwrap(), b"old");
    assert_eq!(names(&dst), ["a.txt"], "no temporary left behind");
}

#[cfg(unix)]
#[test]
fn a_destination_symlinked_to_the_source_is_refused_before_anything_is_created() {
    let d = TempDir::new().unwrap();
    let (src, dst) = (d.path().join("src"), d.path().join("dst"));
    std::fs::create_dir(&src).unwrap();
    std::fs::write(src.join("a.txt"), b"A").unwrap();
    std::os::unix::fs::symlink(&src, &dst).unwrap();
    let before = names(&src);

    let (r, got) = run(&src, &dst);

    assert_eq!(r.unwrap_err().code(), Code::SafetyRejected);
    assert!(got.is_empty());
    assert_eq!(names(&src), before, "nothing was written into the source");
}
