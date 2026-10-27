// Copyright (c) 2026 Costas Kirgoussios
//
// This software is licensed under the PolyForm Noncommercial License 1.0.0.
// You may obtain a copy of the License at:
// https://polyformproject.org/licenses/noncommercial/1.0.0/

//! Cut 13: copy-side verification against a REAL filesystem, on every CI leg. The engine's rules are pinned against
//! `FaultFs` in `src/copy.rs`; these tests drive `copy_file` with `StdFileSystem`, so the destination re-read goes
//! through a second, real handle on the unsynced temporary.

use flux_core::copy_file;
use flux_fs::{
    CopyOptions, Durability, ExistingPolicy, FileSystem, Hardlinks, HashAlgorithm, Links,
    OperationId, Preserve, Publish, Safety, VerifyLevel,
};
use flux_platform::StdFileSystem;
use std::io::{Read, Write};
use tempfile::TempDir;

fn opts(verify: VerifyLevel) -> CopyOptions {
    CopyOptions {
        preserve_times: Preserve::Default,
        preserve_permissions: Preserve::Default,
        durability: Durability::Normal,
        publish: Publish::Replace,
        safety: Safety::Default,
        operation_id: OperationId::new("t"),
        existing: ExistingPolicy::Overwrite,
        links: Links::Copy,
        hardlinks: Hardlinks::Auto,
        cross_filesystems: false,
        verify,
        hash: HashAlgorithm::Blake3,
        selection: flux_fs::Selection::default(),
    }
}

/// 1 MiB + 3 bytes: not a multiple of any buffer or chunk size, with bytes that differ along the file.
fn payload() -> Vec<u8> {
    (0..(1024 * 1024 + 3)).map(|i: usize| (i.wrapping_mul(31) % 251) as u8).collect()
}

#[test]
fn destination_verification_passes_on_a_real_filesystem() {
    let data = payload();
    for level in [VerifyLevel::Destination, VerifyLevel::Full] {
        let d = TempDir::new().unwrap();
        let (src, dst) = (d.path().join("src"), d.path().join("dst"));
        std::fs::write(&src, &data).unwrap();

        let out = copy_file(&StdFileSystem, &src, &dst, &opts(level))
            .unwrap_or_else(|e| panic!("{level:?}: {:?}", e.cause));

        assert!(out.verified, "{level:?}: the copy must report itself verified");
        assert!(!out.skipped, "{level:?}");
        assert_eq!(out.bytes_copied, data.len() as u64, "{level:?}");
        assert_eq!(std::fs::read(&dst).unwrap(), data, "{level:?}: the destination is published");
    }
}

/// Design C14: the destination writer has no user-space buffer, so a second handle opened on the still-unsynced file
/// sees every written byte. This is what the `destination` re-read relies on.
#[test]
fn a_second_handle_sees_every_written_byte() {
    let data = payload();
    let d = TempDir::new().unwrap();
    let path = d.path().join("t");

    let mut w = StdFileSystem.create_new(&path).unwrap();
    w.write_all(&data).unwrap();
    // No sync_all, no drop: the writer is still open and unsynced.

    let mut back = Vec::new();
    StdFileSystem.open_read(&path).unwrap().read_to_end(&mut back).unwrap();
    assert_eq!(back.len(), data.len());
    assert!(back == data, "the second handle read different bytes than were written");
    drop(w);
}
