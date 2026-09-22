//! Single-file copy. The algorithm lives here once; platforms supply primitives.

use flux_fs::{
    Code, CopyOptions, Durability, FileHandle, FileSystem, FsError, MetadataFailure, MetadataItem,
    Outcome, Preserve, Publish, Result, temp_path,
};
use std::io::Write;
use std::path::Path;

/// The code for a failure inside the streaming loop.
///
/// The loop IS the content copy, and the spec defines `COPY_FAILED` as "the content
/// copy of an independent file, or a canonical attempt, failed", so an unclassified
/// failure here is `COPY_FAILED` rather than the `IO_ERROR` catch-all. A failure the
/// platform layer could classify -- a full disk, a denial -- keeps that code, which is
/// more specific than either.
///
/// `classify` borrows the error rather than consuming it: rebuilding one from
/// `e.kind()` would discard `raw_os_error()`, and ENOSPC would stop reaching
/// `DISK_FULL` on the one path where a full disk actually appears.
fn copy_code(e: &std::io::Error) -> Code {
    match FsError::classify(e) {
        Code::IoError => Code::CopyFailed,
        classified => classified,
    }
}

/// Remove the temporary after a failure and build the error to return.
///
/// The removal is never discarded: if the temporary survives, its path goes into the
/// error, because a leftover the caller is never told about is a leak the user cannot
/// even find. Only called where the temporary is known to exist.
fn discard<F: FileSystem>(fs: &F, temp: &Path, code: Code, source: std::io::Error) -> FsError {
    match fs.remove_file(temp) {
        Ok(()) => FsError::new(code, source),
        // NotFound means it is already gone -- something else removed it, or it was
        // never created. Reporting a leftover here would send someone hunting a file
        // that does not exist.
        Err(e) if e.source.kind() == std::io::ErrorKind::NotFound => FsError::new(code, source),
        // Carry the removal's own error too: "a temporary was left" without "because
        // the volume went away" tells an operator where to look but not what happened.
        Err(why) => FsError::new(
            code,
            std::io::Error::other(format!(
                "{source}; temporary left at {} ({})",
                temp.display(),
                why.source
            )),
        ),
    }
}

pub fn copy_file<F: FileSystem>(
    fs: &F,
    src: &Path,
    dst: &Path,
    opts: &CopyOptions,
) -> Result<Outcome> {
    // 0. refuse a self-copy BEFORE touching the filesystem. This must precede every
    //    call below, including the leftover sweep, because
    //    `a_self_copy_is_refused_before_anything_is_touched` asserts the recorded
    //    call list is empty.
    //
    //    §2 Foundational Invariants item 22 asks for identity, not a lexical
    //    comparison: "Safety checks use filesystem identity and object identity where
    //    available, not only lexical path comparisons." This cut compares paths only,
    //    so `flux copy a ./a` is not caught here. It is not destructive -- the copy
    //    goes through a distinct temporary -- but it is not the refusal the invariant
    //    asks for either. Task 10 records the gap.
    if src == dst {
        return Err(FsError::new(
            Code::SafetyRejected,
            std::io::Error::other("source and destination are the same path"),
        ));
    }

    let temp = temp_path(dst, &opts.operation_id);

    // 1. this invocation's own leftover, if any (§18.1). A no-op in this cut: the
    //    operation id is generated per invocation and never persisted, so nothing from
    //    an earlier run carries this name. It stays because §18.1 asks the id to be
    //    "deterministic enough for discovery", and a derivable id makes this live.
    let _ = fs.remove_file(&temp);

    // 2. source, captured for the step-7 re-check
    let src_meta = fs.metadata(src)?;
    if !src_meta.is_file {
        return Err(FsError::new(
            Code::SpecialFileUnsupported,
            std::io::Error::other("not a regular file"),
        ));
    }
    let mut reader = fs.open_read(src)?;

    // 3. exclusive create (FS-1)
    let mut writer = fs.create_new(&temp)?;

    // 4. stream
    let mut bytes_copied = 0u64;
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = match std::io::Read::read(&mut reader, &mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) => return Err(discard(fs, &temp, copy_code(&e), e)),
        };
        if let Err(e) = writer.write_all(&buf[..n]) {
            return Err(discard(fs, &temp, copy_code(&e), e));
        }
        bytes_copied += n as u64;
    }

    // 5. durability
    if opts.durability == Durability::Strict
        && let Err(e) = writer.sync_all()
    {
        return Err(discard(fs, &temp, Code::StrictDurabilityUnavailable, e.source));
    }

    // 6. metadata, on the temporary, BEFORE publication (§44.1)
    let mut metadata_failures = Vec::new();

    if opts.preserve_times != Preserve::Off
        && let Err(e) = fs.set_times(&writer, src_meta.modified)
    {
        if opts.preserve_times == Preserve::Strict {
            return Err(discard(fs, &temp, Code::MetadataApplyFailed, e.source));
        }
        metadata_failures.push(MetadataFailure { item: MetadataItem::Times, error: e });
    }

    if opts.preserve_permissions != Preserve::Off
        && let Err(e) = fs.set_permissions(&writer, src_meta.permissions)
    {
        if opts.preserve_permissions == Preserve::Strict {
            return Err(discard(fs, &temp, Code::MetadataApplyFailed, e.source));
        }
        metadata_failures.push(MetadataFailure { item: MetadataItem::Permissions, error: e });
    }

    // 7. the source must not have changed under us (Section 33), then publish.
    //    Note the `match` rather than `?`: a `?` here would return with the temporary
    //    still on disk, which is the one leak the `discard` helper exists to prevent.
    let now = match fs.metadata(src) {
        Ok(m) => m,
        // Gone is the most complete form of "changed under us", and the taxonomy has
        // a code for that. Reporting IO_ERROR here would hide it among unrelated
        // failures.
        Err(e) if e.source.kind() == std::io::ErrorKind::NotFound => {
            let gone = std::io::Error::other("source disappeared during the copy");
            return Err(discard(fs, &temp, Code::SourceChanged, gone));
        }
        Err(e) => return Err(discard(fs, &temp, e.code, e.source)),
    };
    if now.len != src_meta.len || now.modified != src_meta.modified {
        let changed = std::io::Error::other("source changed");
        return Err(discard(fs, &temp, Code::SourceChanged, changed));
    }

    let published = match opts.publish {
        Publish::Replace => fs.rename_replace(&temp, dst),
        Publish::NoReplace => fs.rename_no_replace(&temp, dst),
    };
    if let Err(e) = published {
        // Keep whatever the platform layer mapped. A publish failure is NOT the content
        // copy -- that already succeeded -- so it must not be relabelled COPY_FAILED,
        // and an unclassified one stays IO_ERROR, the declared catch-all.
        return Err(discard(fs, &temp, e.code, e.source));
    }

    // A successful rename consumed the temporary; there is nothing left to remove.

    Ok(Outcome { bytes_copied, metadata_failures })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fault_fs::FaultFs;
    use flux_fs::{Durability, OperationId, Preserve, Publish};

    fn opts() -> CopyOptions {
        CopyOptions {
            preserve_times: Preserve::Default,
            preserve_permissions: Preserve::Default,
            durability: Durability::Normal,
            publish: Publish::Replace,
            operation_id: OperationId::new("op1"),
        }
    }

    #[test]
    fn a_clean_copy_publishes_and_reports_nothing() {
        let fs = FaultFs::new();
        fs.write_file("/src", b"hello");
        let out = copy_file(&fs, Path::new("/src"), Path::new("/dst"), &opts()).unwrap();

        assert_eq!(out.bytes_copied, 5);
        assert!(out.metadata_failures.is_empty());
        assert!(fs.called("rename_replace"));
        // The CONTENT, not just the call. Asserting only that a rename happened
        // passes just as well against a copy that published nothing.
        assert_eq!(fs.read_file("/dst").as_deref(), Some(&b"hello"[..]));
        assert!(!fs.exists("/dst.flux-partial.op1"));
    }

    #[test]
    fn metadata_is_applied_before_publication() {
        // The whole design in one assertion: §44.1 can only be honoured if the
        // metadata calls precede the rename.
        let fs = FaultFs::new();
        fs.write_file("/src", b"hello");
        copy_file(&fs, Path::new("/src"), Path::new("/dst"), &opts()).unwrap();

        let calls = fs.calls();
        let meta = calls.iter().position(|c| c.starts_with("set_times")).expect("set_times");
        let publish = calls.iter().position(|c| c.starts_with("rename_replace")).expect("rename");
        assert!(meta < publish, "metadata must precede publication; got {calls:?}");
    }

    #[test]
    fn the_temporary_carries_the_operation_id() {
        let fs = FaultFs::new();
        fs.write_file("/src", b"hello");
        copy_file(&fs, Path::new("/src"), Path::new("/dst"), &opts()).unwrap();
        assert!(fs.called("create_new(/dst.flux-partial.op1)"), "{:?}", fs.calls());
    }
}
