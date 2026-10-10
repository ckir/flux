//! Workspace-level integration tests: the ENGINE on the real filesystem.
//!
//! Every case drives `flux_core::run::tree` / `run::file` through `flux_platform::StdFileSystem` in a temp
//! directory and checks the bytes that landed. There is no binary here on purpose: the root package has none,
//! and a test that located `target/debug/flux` could run a stale build after a source change (measured:
//! `cargo test -p flux` does not rebuild it).
//!
//! RULE: a case that needs the binary (flags, exit codes, report text, JSON keys, kill/stall) belongs in
//! `crates/flux-cli/tests`, which cargo builds fresh through `CARGO_BIN_EXE_flux`.
//!
//! Spec §68.2 enumerates the required cases; the ones below are done, the rest are added here as they are
//! written: single file, directory, nested directory, multiple sources, existing destination, overwrite,
//! update, skip-existing, verification, timestamps, permissions, symlinks, Unicode, spaces, newlines,
//! zero-byte files, large files, sparse files, hardlinks, excluded hardlink members, resume, interrupted copy,
//! reflink, dry-run, JSON, destination-inside-source, mount boundaries, disk full, stale operation cleanup,
//! operation lock contention.
//!
//! Done: nested directory, Unicode / space / newline names, zero-byte files, byte-for-byte verification.

mod support;

use flux_fs::Durability;
use std::fs;
use std::path::Path;
use support::{copy_tree, copy_tree_with, tree_bytes};

fn write(root: &Path, rel: &str, bytes: &[u8]) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, bytes).unwrap();
}

/// A deterministic, non-repeating payload, so a truncated or shifted copy cannot compare equal.
fn payload(seed: u8, len: usize) -> Vec<u8> {
    (0..len).map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed) ^ ((i >> 8) as u8)).collect()
}

#[test]
fn nested_directory_arrives_byte_for_byte() {
    let t = tempfile::tempdir().unwrap();
    let (src, dst) = (t.path().join("src"), t.path().join("dst"));
    write(&src, "a.bin", &payload(1, 70_000));
    write(&src, "d1/b.bin", &payload(2, 4096));
    write(&src, "d1/d2/d3/c.bin", &payload(3, 1));
    fs::create_dir_all(src.join("d1/empty")).unwrap();

    let out = copy_tree(&src, &dst);
    assert_eq!(out.files_copied, 3);
    assert_eq!(tree_bytes(&dst), tree_bytes(&src));
    assert!(dst.join("d1/empty").is_dir(), "an empty directory is part of the tree");
}

#[test]
fn unicode_space_and_newline_names_survive() {
    let t = tempfile::tempdir().unwrap();
    let (src, dst) = (t.path().join("src"), t.path().join("dst"));
    let names = ["naïve café.txt", "日本語/ファイル.txt", "with space/a b.txt", "emoji-🦀.bin"];
    for (i, n) in names.iter().enumerate() {
        write(&src, n, &payload(i as u8, 100 + i));
    }
    // A newline in a name is legal on Unix and refused by Windows and macOS-style tooling.
    #[cfg(unix)]
    write(&src, "line\nbreak.txt", b"newline in the name");

    let out = copy_tree(&src, &dst);
    let files = tree_bytes(&src).keys().filter(|k| !k.ends_with('/')).count();
    assert_eq!(out.files_copied as usize, files);
    assert_eq!(tree_bytes(&dst), tree_bytes(&src));
    for n in names {
        assert!(dst.join(n).is_file(), "{n} must exist at the destination");
    }
}

#[test]
fn zero_byte_files_are_created_not_skipped() {
    let t = tempfile::tempdir().unwrap();
    let (src, dst) = (t.path().join("src"), t.path().join("dst"));
    write(&src, "empty.txt", b"");
    write(&src, "sub/also-empty", b"");
    write(&src, "sub/full", b"x");

    let out = copy_tree(&src, &dst);
    assert_eq!(out.files_copied, 3);
    assert_eq!(fs::metadata(dst.join("empty.txt")).unwrap().len(), 0);
    assert_eq!(fs::metadata(dst.join("sub/also-empty")).unwrap().len(), 0);
    assert_eq!(tree_bytes(&dst), tree_bytes(&src));
}

#[test]
fn single_file_copy_is_byte_for_byte() {
    let t = tempfile::tempdir().unwrap();
    let (src, dst) = (t.path().join("one.bin"), t.path().join("copy.bin"));
    let data = payload(9, 1 << 20);
    fs::write(&src, &data).unwrap();

    support::copy_file(&src, &dst);
    assert_eq!(blake3::hash(&fs::read(&dst).unwrap()), blake3::hash(&data));
    assert_eq!(fs::read(&src).unwrap(), data, "the source is untouched");
}

/// Cut 9d group commit on the real filesystem: a Strict copy batches 64 files per directory, so 70 files split 64 + 6.
/// Each directory also holds a zero-byte file and a 300 KiB file (both among its 70).
#[test]
fn strict_tree_of_many_files_arrives_byte_for_byte() {
    let t = tempfile::tempdir().unwrap();
    let (src, dst) = (t.path().join("src"), t.path().join("dst"));
    for d in 0..3u8 {
        for f in 0..68u8 {
            write(&src, &format!("d{d}/f{f:02}"), &payload(d * 70 + f, 100 + f as usize));
        }
        write(&src, &format!("d{d}/zero"), b"");
        write(&src, &format!("d{d}/big"), &payload(d, 300 * 1024));
    }

    let out = copy_tree_with(&src, &dst, Durability::Strict);
    assert_eq!(out.files_copied, 210);
    assert_eq!(tree_bytes(&dst), tree_bytes(&src));
    assert!(!dst.join(".flux").exists(), "the finished copy removes its workspace");
    fn leftovers(dir: &Path, found: &mut Vec<String>) {
        for e in fs::read_dir(dir).unwrap() {
            let e = e.unwrap();
            let name = e.file_name().to_string_lossy().into_owned();
            if name.contains(".flux-partial.") {
                found.push(name);
            }
            if e.file_type().unwrap().is_dir() {
                leftovers(&e.path(), found);
            }
        }
    }
    let mut found = Vec::new();
    leftovers(&dst, &mut found);
    assert!(found.is_empty(), "no temporary survives: {found:?}");
}
