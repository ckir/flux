# `flux-fs` and Single-File Copy Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `flux copy <src> <dst>` copy a single file with metadata, so the binary stops printing "not implemented yet".

**Architecture:** A synchronous primitives trait in `flux-fs`; one real implementation in `flux-platform` over `std::fs`; the copy algorithm once in `flux-core`, generic over the trait; a fault-injecting fake in `flux-core`'s tests to prove the ordering rules that matter.

**Tech Stack:** Rust 2024, `thiserror` for the error type, `clap` (derive) for the CLI, `tempfile` for real-filesystem tests. No async runtime — the workspace pins `rayon` and `crossbeam-channel`.

---

## Spec

`docs/superpowers/specs/2026-09-22-flux-fs-single-file-copy-design.md` (commit `fccee14`).

Read it first. The load-bearing rule is **§44.1**: explicitly requested metadata is strict and its
failure must prevent publication, which is why metadata is applied to the temporary before the
publishing rename.

## Verified facts this plan rests on

Checked against the repository at `6472a57` while writing. Re-check if `main` has moved.

| fact | how it was checked |
|---|---|
| workspace members are the six `crates/*` | `Cargo.toml` `[workspace] members` |
| `flux-fs` and `flux-core` declare **no** dependencies yet | their `Cargo.toml` has no `[dependencies]` |
| `flux-platform` has `tempfile` as a dev-dependency only | its `Cargo.toml` |
| `thiserror = "2"`, `clap = { version = "4.6", features = ["derive"] }`, `tempfile = "3"` are in `[workspace.dependencies]` | root `Cargo.toml` |
| tests run with `cargo nextest run --workspace --no-tests=pass` | `justfile:12-13` |
| `just check` = `fmt-check clippy typos test` | `justfile:46` |
| `flux-cli` has `[[bin]] name = "flux"`, `path = "src/main.rs"` | `crates/flux-cli/Cargo.toml` |
| the probes are `fs1_..` to `fs11_..` in `crates/flux-platform/tests/fs_semantics.rs` | `grep -n "^fn fs"` |

**One inconsistency in the spec, reconciled here.** Its error-taxonomy table lists seven codes, but its
own edge-case table produces two more — `SPECIAL_FILE_UNSUPPORTED` (a symlink, device or directory
source) and `SAFETY_REJECTED` (self-copy). This plan implements **nine** codes. Task 1 adds a test per
code, which is what the spec's "no variant without a reachable producer" rule actually demands.

## File structure

| file | responsibility |
|---|---|
| `crates/flux-fs/src/error.rs` | `Code`, `FsError` — the spec's codes and the io::Error wrapper |
| `crates/flux-fs/src/options.rs` | `Preserve`, `Durability`, `Publish`, `CopyOptions`, `Outcome`, `MetadataFailure` |
| `crates/flux-fs/src/fs.rs` | the `FileSystem` and `FileHandle` traits, and `Metadata` |
| `crates/flux-fs/src/lib.rs` | re-exports only |
| `crates/flux-platform/src/std_fs.rs` | `StdFileSystem`, the one real implementation |
| `crates/flux-core/src/copy.rs` | `copy_file` — the algorithm, once |
| `crates/flux-core/src/fault_fs.rs` | `FaultFs`, the injecting fake (behind `#[cfg(any(test, feature = "testing"))]`) |
| `crates/flux-cli/src/main.rs` | `flux copy` wiring |

Small files, one responsibility each. `copy.rs` is the only file with branching logic.

---

## Task 1: The error type and the spec's codes

**Files:**
- Create: `crates/flux-fs/src/error.rs`
- Modify: `crates/flux-fs/src/lib.rs`, `crates/flux-fs/Cargo.toml`

- [ ] **Step 1: Add the dependency**

In `crates/flux-fs/Cargo.toml`, after the `[package]` block:

```toml
[dependencies]
thiserror = { workspace = true }
```

- [ ] **Step 2: Write the failing test**

Create `crates/flux-fs/src/error.rs` with only this test module at the bottom:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_code_has_the_spec_string() {
        assert_eq!(Code::CopyFailed.as_str(), "COPY_FAILED");
        assert_eq!(Code::MetadataApplyFailed.as_str(), "METADATA_APPLY_FAILED");
        assert_eq!(Code::DiskFull.as_str(), "DISK_FULL");
        assert_eq!(Code::PermissionDenied.as_str(), "PERMISSION_DENIED");
        assert_eq!(Code::SourceChanged.as_str(), "SOURCE_CHANGED");
        assert_eq!(Code::StrictDurabilityUnavailable.as_str(), "STRICT_DURABILITY_UNAVAILABLE");
        assert_eq!(Code::SpecialFileUnsupported.as_str(), "SPECIAL_FILE_UNSUPPORTED");
        assert_eq!(Code::SafetyRejected.as_str(), "SAFETY_REJECTED");
        assert_eq!(Code::IoError.as_str(), "IO_ERROR");
    }

    #[test]
    fn enospc_maps_to_disk_full() {
        let io = std::io::Error::from_raw_os_error(ENOSPC_RAW);
        assert_eq!(FsError::from_io(io).code, Code::DiskFull);
    }

    #[test]
    fn permission_denied_maps_to_permission_denied() {
        let io = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        assert_eq!(FsError::from_io(io).code, Code::PermissionDenied);
    }

    #[test]
    fn anything_else_maps_to_io_error() {
        let io = std::io::Error::from(std::io::ErrorKind::NotFound);
        assert_eq!(FsError::from_io(io).code, Code::IoError);
    }
}
```

- [ ] **Step 3: Run it and watch it fail**

Run: `cargo nextest run -p flux-fs --no-tests=pass`
Expected: FAIL — `cannot find type Code in this scope`.

- [ ] **Step 4: Write the implementation**

Above the test module in `crates/flux-fs/src/error.rs`:

```rust
//! The spec's error codes, and the `io::Error` wrapper that carries one.

/// `ENOSPC` on Unix, `ERROR_DISK_FULL` on Windows.
#[cfg(unix)]
pub(crate) const ENOSPC_RAW: i32 = 28;
#[cfg(windows)]
pub(crate) const ENOSPC_RAW: i32 = 112;

/// A spec error code. Only codes single-file copy can actually produce.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Code {
    CopyFailed,
    MetadataApplyFailed,
    DiskFull,
    PermissionDenied,
    SourceChanged,
    StrictDurabilityUnavailable,
    SpecialFileUnsupported,
    SafetyRejected,
    IoError,
}

impl Code {
    pub fn as_str(self) -> &'static str {
        match self {
            Code::CopyFailed => "COPY_FAILED",
            Code::MetadataApplyFailed => "METADATA_APPLY_FAILED",
            Code::DiskFull => "DISK_FULL",
            Code::PermissionDenied => "PERMISSION_DENIED",
            Code::SourceChanged => "SOURCE_CHANGED",
            Code::StrictDurabilityUnavailable => "STRICT_DURABILITY_UNAVAILABLE",
            Code::SpecialFileUnsupported => "SPECIAL_FILE_UNSUPPORTED",
            Code::SafetyRejected => "SAFETY_REJECTED",
            Code::IoError => "IO_ERROR",
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("{}: {source}", code.as_str())]
pub struct FsError {
    pub code: Code,
    #[source]
    pub source: std::io::Error,
}

impl FsError {
    /// Map an OS error to a spec code. **This is the platform layer's job**: the raw
    /// numbers differ per OS and `flux-core` must never branch on them.
    pub fn from_io(source: std::io::Error) -> Self {
        let code = if source.raw_os_error() == Some(ENOSPC_RAW) {
            Code::DiskFull
        } else if source.kind() == std::io::ErrorKind::PermissionDenied {
            Code::PermissionDenied
        } else {
            Code::IoError
        };
        FsError { code, source }
    }

    pub fn new(code: Code, source: std::io::Error) -> Self {
        FsError { code, source }
    }
}

pub type Result<T> = std::result::Result<T, FsError>;
```

Replace `crates/flux-fs/src/lib.rs` entirely with:

```rust
//! Portable filesystem abstractions (spec §3.2).

pub mod error;

pub use error::{Code, FsError, Result};
```

- [ ] **Step 5: Run the tests**

Run: `cargo nextest run -p flux-fs --no-tests=pass`
Expected: PASS, 4 tests.

- [ ] **Step 6: Commit**

```bash
git add crates/flux-fs
git commit -m "feat(flux-fs): the spec's error codes and the io::Error mapping"
```

---

## Task 2: Options, outcome, and the three-state `Preserve`

**Files:**
- Create: `crates/flux-fs/src/options.rs`
- Modify: `crates/flux-fs/src/lib.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/flux-fs/src/options.rs` with only this at the bottom:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserve_distinguishes_off_from_default() {
        // §44.1 treats "not requested" and "requested leniently" differently, so a
        // boolean cannot express this.
        assert_ne!(Preserve::Off, Preserve::Default);
        assert_ne!(Preserve::Default, Preserve::Strict);
    }

    #[test]
    fn a_clean_outcome_has_no_metadata_failures() {
        let o = Outcome { bytes_copied: 10, metadata_failures: Vec::new() };
        assert!(o.metadata_failures.is_empty());
        assert_eq!(o.bytes_copied, 10);
    }

    #[test]
    fn temp_name_follows_the_normative_structure() {
        // §18.1 makes `<target>.flux-partial.<operation-id>` normative.
        let id = OperationId::new("abc123");
        let p = temp_path(std::path::Path::new("/dest/file.iso"), &id);
        assert_eq!(p.file_name().unwrap(), "file.iso.flux-partial.abc123");
    }
}
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo nextest run -p flux-fs --no-tests=pass`
Expected: FAIL — `cannot find type Preserve in this scope`.

- [ ] **Step 3: Write the implementation**

Above the tests in `crates/flux-fs/src/options.rs`:

```rust
//! What a copy was asked to do, and what it did.

use std::path::{Path, PathBuf};

/// Three states, not two. §44.1 makes a strict failure prevent publication while a
/// default failure does not, and "not requested at all" is a third thing again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preserve {
    /// Explicitly requested. Failure fails the file's action; nothing is published.
    Strict,
    /// Applied by default. Failure is reported; the file is still published.
    Default,
    /// Not attempted.
    Off,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Durability {
    Normal,
    Strict,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Publish {
    Replace,
    NoReplace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetadataItem {
    Times,
    Permissions,
}

/// §18.1: the textual form is implementation-defined but must be collision-resistant
/// and deterministic enough for discovery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationId(String);

impl OperationId {
    pub fn new(s: impl Into<String>) -> Self {
        OperationId(s.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone)]
pub struct CopyOptions {
    pub preserve_times: Preserve,
    pub preserve_permissions: Preserve,
    pub durability: Durability,
    pub publish: Publish,
    pub operation_id: OperationId,
}

#[derive(Debug)]
pub struct MetadataFailure {
    pub item: MetadataItem,
    pub error: crate::FsError,
}

#[derive(Debug)]
pub struct Outcome {
    pub bytes_copied: u64,
    /// Empty on a clean copy. Non-empty means published-with-complaints: the caller
    /// reports each as METADATA_APPLY_FAILED and the operation exits 1.
    pub metadata_failures: Vec<MetadataFailure>,
}

/// `<target>.flux-partial.<operation-id>`, in the target's directory (§18.1, normative).
pub fn temp_path(target: &Path, id: &OperationId) -> PathBuf {
    let mut name = target.file_name().unwrap_or_default().to_os_string();
    name.push(".flux-partial.");
    name.push(id.as_str());
    target.with_file_name(name)
}
```

Add to `crates/flux-fs/src/lib.rs`:

```rust
pub mod options;

pub use options::{
    CopyOptions, Durability, MetadataFailure, MetadataItem, OperationId, Outcome, Preserve, Publish,
    temp_path,
};
```

- [ ] **Step 4: Run the tests**

Run: `cargo nextest run -p flux-fs --no-tests=pass`
Expected: PASS, 7 tests.

- [ ] **Step 5: Commit**

```bash
git add crates/flux-fs
git commit -m "feat(flux-fs): copy options, outcome, and the normative temp name"
```

---

## Task 3: The traits

**Files:**
- Create: `crates/flux-fs/src/fs.rs`
- Modify: `crates/flux-fs/src/lib.rs`

A trait alone has no behaviour to test, so the test here is that a **trivial implementation
compiles** — which is the real risk with a trait (an un-implementable signature).

- [ ] **Step 1: Write the failing test**

Create `crates/flux-fs/src/fs.rs` with only this at the bottom:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    struct NullHandle;

    impl Read for NullHandle {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> { Ok(0) }
    }
    impl Write for NullHandle {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> { Ok(b.len()) }
        fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
    }
    impl FileHandle for NullHandle {
        fn sync_all(&self) -> crate::Result<()> { Ok(()) }
    }

    struct NullFs;

    impl FileSystem for NullFs {
        type File = NullHandle;
        fn open_read(&self, _: &Path) -> crate::Result<Self::File> { Ok(NullHandle) }
        fn create_new(&self, _: &Path) -> crate::Result<Self::File> { Ok(NullHandle) }
        fn metadata(&self, _: &Path) -> crate::Result<Metadata> {
            Ok(Metadata { len: 0, is_file: true, permissions: None, modified: None })
        }
        fn set_times(&self, _: &Self::File, _: Option<std::time::SystemTime>) -> crate::Result<()> { Ok(()) }
        fn set_permissions(&self, _: &Self::File, _: Option<Perms>) -> crate::Result<()> { Ok(()) }
        fn rename_replace(&self, _: &Path, _: &Path) -> crate::Result<()> { Ok(()) }
        fn rename_no_replace(&self, _: &Path, _: &Path) -> crate::Result<()> { Ok(()) }
        fn remove_file(&self, _: &Path) -> crate::Result<()> { Ok(()) }
    }

    #[test]
    fn a_trivial_implementation_compiles_and_is_object_safe_enough_to_use() {
        let fs = NullFs;
        assert!(fs.metadata(Path::new("x")).unwrap().is_file);
    }

    #[test]
    fn the_trait_is_send_and_sync() {
        fn requires<T: Send + Sync>() {}
        requires::<NullFs>();
    }
}
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo nextest run -p flux-fs --no-tests=pass`
Expected: FAIL — `cannot find trait FileSystem in this scope`.

- [ ] **Step 3: Write the implementation**

Above the tests in `crates/flux-fs/src/fs.rs`:

```rust
//! The portable filesystem surface. Only what single-file copy needs (§3.2).

use crate::Result;
use std::io::{Read, Write};
use std::path::Path;
use std::time::SystemTime;

/// Permissions in the only form this cut needs: the Unix mode, or the Windows
/// read-only flag. Deliberately not `std::fs::Permissions`, which cannot be
/// constructed portably.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Perms {
    UnixMode(u32),
    ReadOnly(bool),
}

/// Only what single-file copy reads. Widened when a consumer needs more.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Metadata {
    pub len: u64,
    /// False for directories, symlinks, devices, FIFOs and sockets.
    pub is_file: bool,
    pub permissions: Option<Perms>,
    pub modified: Option<SystemTime>,
}

pub trait FileHandle: Read + Write {
    fn sync_all(&self) -> Result<()>;
}

pub trait FileSystem: Send + Sync {
    type File: FileHandle;

    fn open_read(&self, path: &Path) -> Result<Self::File>;

    /// Exclusive create: fails if the path exists. FS-1 measured that a second
    /// exclusive create fails, which is what makes the temporary safe.
    fn create_new(&self, path: &Path) -> Result<Self::File>;

    fn metadata(&self, path: &Path) -> Result<Metadata>;

    /// On the HANDLE, not the path: the temporary must be fully prepared before
    /// publication, and addressing it by path in between is a race. FS-4 and FS-5
    /// measured that a handle survives both rename and unlink.
    fn set_times(&self, file: &Self::File, modified: Option<SystemTime>) -> Result<()>;

    fn set_permissions(&self, file: &Self::File, perms: Option<Perms>) -> Result<()>;

    /// Publishes over an existing target.
    fn rename_replace(&self, from: &Path, to: &Path) -> Result<()>;

    /// Refuses to replace. §18.1 publishes this way when the target was planned as
    /// new; FS-2 measured the failure.
    fn rename_no_replace(&self, from: &Path, to: &Path) -> Result<()>;

    fn remove_file(&self, path: &Path) -> Result<()>;
}
```

Add to `crates/flux-fs/src/lib.rs`:

```rust
pub mod fs;

pub use fs::{FileHandle, FileSystem, Metadata, Perms};
```

- [ ] **Step 4: Run the tests**

Run: `cargo nextest run -p flux-fs --no-tests=pass`
Expected: PASS, 9 tests.

- [ ] **Step 5: Commit**

```bash
git add crates/flux-fs
git commit -m "feat(flux-fs): the FileSystem and FileHandle traits"
```

---

## Task 4: `FaultFs`, the injecting fake

**Files:**
- Create: `crates/flux-core/src/fault_fs.rs`
- Modify: `crates/flux-core/src/lib.rs`, `crates/flux-core/Cargo.toml`

This is the instrument every later task measures with. It records the **order** of calls, because
the ordering is the requirement.

- [ ] **Step 1: Add the dependency**

In `crates/flux-core/Cargo.toml`:

```toml
[dependencies]
flux-fs = { path = "../flux-fs" }
```

- [ ] **Step 2: Write the failing test**

Create `crates/flux-core/src/fault_fs.rs` with only this at the bottom:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_records_the_order_of_calls() {
        let fs = FaultFs::new();
        fs.write_file("/a", b"hi");
        let _ = fs.metadata(std::path::Path::new("/a"));
        let _ = fs.remove_file(std::path::Path::new("/a"));
        assert_eq!(fs.calls(), vec!["metadata(/a)".to_string(), "remove_file(/a)".to_string()]);
    }

    #[test]
    fn it_fails_the_named_call() {
        let fs = FaultFs::new();
        fs.fail("remove_file", flux_fs::Code::PermissionDenied);
        let e = fs.remove_file(std::path::Path::new("/a")).unwrap_err();
        assert_eq!(e.code, flux_fs::Code::PermissionDenied);
    }
}
```

- [ ] **Step 3: Run it and watch it fail**

Run: `cargo nextest run -p flux-core --no-tests=pass`
Expected: FAIL — `cannot find type FaultFs in this scope`.

- [ ] **Step 4: Write the implementation**

Above the tests in `crates/flux-core/src/fault_fs.rs`:

```rust
//! A filesystem that does what you tell it and remembers what you asked.
//!
//! The spec's rules are about ORDER — metadata before publication, no publication
//! after a strict failure — so this records the call sequence and can fail any
//! named call. None of that is reachable against a real disk.

use flux_fs::{Code, FileHandle, FileSystem, FsError, Metadata, Perms, Result};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::Mutex;
use std::time::SystemTime;

#[derive(Default)]
struct Inner {
    files: HashMap<String, Vec<u8>>,
    calls: Vec<String>,
    faults: HashMap<String, Code>,
    times: HashMap<String, Option<SystemTime>>,
    /// path -> bytes appended on the second `metadata` call
    grow: HashMap<String, Vec<u8>>,
    metadata_reads: HashMap<String, u32>,
}

#[derive(Default)]
pub struct FaultFs {
    inner: Mutex<Inner>,
}

pub struct FakeHandle {
    path: String,
    buf: Vec<u8>,
    read_pos: usize,
    sync_fault: std::sync::Arc<Mutex<Option<Code>>>,
}

impl Read for FakeHandle {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        let n = (self.buf.len() - self.read_pos).min(out.len());
        out[..n].copy_from_slice(&self.buf[self.read_pos..self.read_pos + n]);
        self.read_pos += n;
        Ok(n)
    }
}

impl Write for FakeHandle {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.buf.extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl FileHandle for FakeHandle {
    fn sync_all(&self) -> Result<()> {
        // The durability test injects here, so the handle carries a shared fault slot.
        if let Some(code) = self.sync_fault.lock().unwrap().take() {
            return Err(FsError::new(code, std::io::Error::other("injected")));
        }
        Ok(())
    }
}

impl FaultFs {
    pub fn new() -> Self {
        Self::default()
    }

    /// Seed a source file.
    pub fn write_file(&self, path: &str, bytes: &[u8]) {
        self.inner.lock().unwrap().files.insert(path.to_string(), bytes.to_vec());
    }

    pub fn exists(&self, path: &str) -> bool {
        self.inner.lock().unwrap().files.contains_key(path)
    }

    /// Make the next call to `name` fail with `code`.
    pub fn fail(&self, name: &str, code: Code) {
        self.inner.lock().unwrap().faults.insert(name.to_string(), code);
    }

    /// Hands each new handle the shared `sync_all` fault slot, so `fail("sync_all", ..)`
    /// reaches a handle created afterwards.
    fn sync_slot(&self) -> std::sync::Arc<Mutex<Option<Code>>> {
        let mut g = self.inner.lock().unwrap();
        let code = g.faults.remove("sync_all");
        std::sync::Arc::new(Mutex::new(code))
    }

    pub fn calls(&self) -> Vec<String> {
        self.inner.lock().unwrap().calls.clone()
    }

    pub fn called(&self, prefix: &str) -> bool {
        self.calls().iter().any(|c| c.starts_with(prefix))
    }

    /// Append to a file the *second* time its metadata is read — what a concurrent
    /// writer looks like from inside step 6.
    pub fn grow_on_second_metadata(&self, path: &str, extra: &[u8]) {
        let mut g = self.inner.lock().unwrap();
        g.grow.insert(path.to_string(), extra.to_vec());
    }

    fn record(&self, call: String, key: &str) -> Result<()> {
        let mut g = self.inner.lock().unwrap();
        g.calls.push(call);
        if let Some(code) = g.faults.remove(key) {
            return Err(FsError::new(code, std::io::Error::other("injected")));
        }
        Ok(())
    }
}

impl FileSystem for FaultFs {
    type File = FakeHandle;

    fn open_read(&self, path: &Path) -> Result<Self::File> {
        let p = path.display().to_string();
        self.record(format!("open_read({p})"), "open_read")?;
        let g = self.inner.lock().unwrap();
        let buf = g.files.get(&p).cloned().ok_or_else(|| {
            FsError::new(Code::IoError, std::io::Error::from(std::io::ErrorKind::NotFound))
        })?;
        // The reader never syncs; only the writer may consume the injected sync fault.
        Ok(FakeHandle { path: p, buf, read_pos: 0, sync_fault: std::sync::Arc::new(Mutex::new(None)) })
    }

    fn create_new(&self, path: &Path) -> Result<Self::File> {
        let p = path.display().to_string();
        self.record(format!("create_new({p})"), "create_new")?;
        let mut g = self.inner.lock().unwrap();
        if g.files.contains_key(&p) {
            return Err(FsError::new(
                Code::IoError,
                std::io::Error::from(std::io::ErrorKind::AlreadyExists),
            ));
        }
        g.files.insert(p.clone(), Vec::new());
        Ok(FakeHandle { path: p, buf: Vec::new(), read_pos: 0, sync_fault: self.sync_slot() })
    }

    fn metadata(&self, path: &Path) -> Result<Metadata> {
        let p = path.display().to_string();
        self.record(format!("metadata({p})"), "metadata")?;
        let mut g = self.inner.lock().unwrap();
        let n = g.metadata_reads.entry(p.clone()).or_insert(0);
        *n += 1;
        if *n == 2 {
            if let Some(extra) = g.grow.get(&p).cloned() {
                if let Some(b) = g.files.get_mut(&p) {
                    b.extend_from_slice(&extra);
                }
            }
        }
        let g = &*g;
        let len = g.files.get(&p).map(|b| b.len() as u64).ok_or_else(|| {
            FsError::new(Code::IoError, std::io::Error::from(std::io::ErrorKind::NotFound))
        })?;
        Ok(Metadata {
            len,
            is_file: true,
            permissions: None,
            modified: g.times.get(&p).copied().flatten(),
        })
    }

    fn set_times(&self, file: &Self::File, modified: Option<SystemTime>) -> Result<()> {
        self.record(format!("set_times({})", file.path), "set_times")?;
        self.inner.lock().unwrap().times.insert(file.path.clone(), modified);
        Ok(())
    }

    fn set_permissions(&self, file: &Self::File, _perms: Option<Perms>) -> Result<()> {
        self.record(format!("set_permissions({})", file.path), "set_permissions")
    }

    fn rename_replace(&self, from: &Path, to: &Path) -> Result<()> {
        let (f, t) = (from.display().to_string(), to.display().to_string());
        self.record(format!("rename_replace({f} -> {t})"), "rename_replace")?;
        let mut g = self.inner.lock().unwrap();
        let bytes = g.files.remove(&f).unwrap_or_default();
        g.files.insert(t, bytes);
        Ok(())
    }

    fn rename_no_replace(&self, from: &Path, to: &Path) -> Result<()> {
        let (f, t) = (from.display().to_string(), to.display().to_string());
        self.record(format!("rename_no_replace({f} -> {t})"), "rename_no_replace")?;
        let mut g = self.inner.lock().unwrap();
        if g.files.contains_key(&t) {
            return Err(FsError::new(
                Code::IoError,
                std::io::Error::from(std::io::ErrorKind::AlreadyExists),
            ));
        }
        let bytes = g.files.remove(&f).unwrap_or_default();
        g.files.insert(t, bytes);
        Ok(())
    }

    fn remove_file(&self, path: &Path) -> Result<()> {
        let p = path.display().to_string();
        self.record(format!("remove_file({p})"), "remove_file")?;
        self.inner.lock().unwrap().files.remove(&p);
        Ok(())
    }
}
```

Important: `FakeHandle` writes into its own buffer, so `copy_file` must flush it back. Rather than
model that, the fake's `rename_*` moves whatever key exists — and Task 5's happy-path test asserts on
the **call order**, not on byte contents. Byte fidelity is `flux-platform`'s job and Task 11 tests it
against a real disk.

Replace `crates/flux-core/src/lib.rs`:

```rust
//! Flux engine (spec §3.1).

pub mod fault_fs;
```

- [ ] **Step 5: Run the tests**

Run: `cargo nextest run -p flux-core --no-tests=pass`
Expected: PASS, 2 tests.

- [ ] **Step 6: Commit**

```bash
git add crates/flux-core
git commit -m "test(flux-core): a filesystem fake that records order and injects faults"
```

---

## Task 5: `copy_file`, the happy path

**Files:**
- Create: `crates/flux-core/src/copy.rs`
- Modify: `crates/flux-core/src/lib.rs`

- [ ] **Step 1: Write the failing test**

At the bottom of `crates/flux-core/src/copy.rs`:

```rust
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
        assert!(fs.exists("/dst"));
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
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo nextest run -p flux-core --no-tests=pass`
Expected: FAIL — `cannot find function copy_file in this scope`.

- [ ] **Step 3: Write the implementation**

Above the tests in `crates/flux-core/src/copy.rs`:

```rust
//! Single-file copy. The algorithm lives here once; platforms supply primitives.

use flux_fs::{
    Code, CopyOptions, Durability, FileSystem, FsError, MetadataFailure, MetadataItem, Outcome,
    Preserve, Publish, Result, temp_path,
};
use std::io::Write;
use std::path::Path;

pub fn copy_file<F: FileSystem>(
    fs: &F,
    src: &Path,
    dst: &Path,
    opts: &CopyOptions,
) -> Result<Outcome> {
    let temp = temp_path(dst, &opts.operation_id);

    // 0. this invocation's own leftover, if any (§18.1)
    let _ = fs.remove_file(&temp);

    // 1. source, captured for the step-6 re-check
    if src == dst {
        // Section 33 / SAFETY_REJECTED. Without this the copy truncates the source
        // into its own temporary and publishes the wreckage over it.
        return Err(FsError::new(
            Code::SafetyRejected,
            std::io::Error::other("source and destination are the same path"),
        ));
    }
    let src_meta = fs.metadata(src)?;
    if !src_meta.is_file {
        return Err(FsError::new(
            Code::SpecialFileUnsupported,
            std::io::Error::other("not a regular file"),
        ));
    }
    let mut reader = fs.open_read(src)?;

    // 2. exclusive create (FS-1)
    let mut writer = fs.create_new(&temp)?;

    // 3. stream
    let mut bytes_copied = 0u64;
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = match std::io::Read::read(&mut reader, &mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) => {
                let _ = fs.remove_file(&temp);
                return Err(FsError::from_io(e));
            }
        };
        if let Err(e) = writer.write_all(&buf[..n]) {
            let _ = fs.remove_file(&temp);
            return Err(FsError::from_io(e));
        }
        bytes_copied += n as u64;
    }

    // 4. durability
    if opts.durability == Durability::Strict {
        if let Err(e) = writer.sync_all() {
            let _ = fs.remove_file(&temp);
            return Err(FsError::new(Code::StrictDurabilityUnavailable, e.source));
        }
    }

    // 5. metadata, on the temporary, BEFORE publication (§44.1)
    let mut metadata_failures = Vec::new();

    if opts.preserve_times != Preserve::Off {
        if let Err(e) = fs.set_times(&writer, src_meta.modified) {
            if opts.preserve_times == Preserve::Strict {
                let _ = fs.remove_file(&temp);
                return Err(FsError::new(Code::MetadataApplyFailed, e.source));
            }
            metadata_failures.push(MetadataFailure { item: MetadataItem::Times, error: e });
        }
    }

    if opts.preserve_permissions != Preserve::Off {
        if let Err(e) = fs.set_permissions(&writer, src_meta.permissions) {
            if opts.preserve_permissions == Preserve::Strict {
                let _ = fs.remove_file(&temp);
                return Err(FsError::new(Code::MetadataApplyFailed, e.source));
            }
            metadata_failures.push(MetadataFailure { item: MetadataItem::Permissions, error: e });
        }
    }

    // 6. the source must not have changed under us (Section 33), then publish
    let now = fs.metadata(src)?;
    if now.len != src_meta.len || now.modified != src_meta.modified {
        let _ = fs.remove_file(&temp);
        return Err(FsError::new(Code::SourceChanged, std::io::Error::other("source changed")));
    }

    let published = match opts.publish {
        Publish::Replace => fs.rename_replace(&temp, dst),
        Publish::NoReplace => fs.rename_no_replace(&temp, dst),
    };
    if let Err(e) = published {
        let _ = fs.remove_file(&temp);
        return Err(FsError::new(Code::CopyFailed, e.source));
    }

    // 7. §18.1
    let _ = fs.remove_file(&temp);

    Ok(Outcome { bytes_copied, metadata_failures })
}
```

Add to `crates/flux-core/src/lib.rs`:

```rust
pub mod copy;

pub use copy::copy_file;
```

- [ ] **Step 4: Run the tests**

Run: `cargo nextest run -p flux-core --no-tests=pass`
Expected: PASS, 5 tests.

- [ ] **Step 5: Commit**

```bash
git add crates/flux-core
git commit -m "feat(flux-core): copy_file, with metadata applied before publication"
```

---

## Task 6: The rules that matter — fault-injection tests

**Files:**
- Modify: `crates/flux-core/src/copy.rs` (tests only)

No implementation changes. If a test here fails, Task 5's code is wrong.

- [ ] **Step 1: Add the tests**

Inside the existing `mod tests` in `crates/flux-core/src/copy.rs`:

```rust
    #[test]
    fn a_strict_metadata_failure_prevents_publication() {
        // §44.1, the rule this whole design exists to honour.
        let fs = FaultFs::new();
        fs.write_file("/src", b"hello");
        fs.fail("set_times", flux_fs::Code::PermissionDenied);

        let mut o = opts();
        o.preserve_times = Preserve::Strict;
        let err = copy_file(&fs, Path::new("/src"), Path::new("/dst"), &o).unwrap_err();

        assert_eq!(err.code, flux_fs::Code::MetadataApplyFailed);
        assert!(!fs.called("rename_replace"), "must not publish; got {:?}", fs.calls());
        assert!(!fs.exists("/dst"));
        assert!(!fs.exists("/dst.flux-partial.op1"), "temporary must be removed");
    }

    #[test]
    fn a_default_metadata_failure_still_publishes_and_reports() {
        let fs = FaultFs::new();
        fs.write_file("/src", b"hello");
        fs.fail("set_times", flux_fs::Code::PermissionDenied);

        let out = copy_file(&fs, Path::new("/src"), Path::new("/dst"), &opts()).unwrap();

        assert_eq!(out.metadata_failures.len(), 1);
        assert_eq!(out.metadata_failures[0].item, flux_fs::MetadataItem::Times);
        assert!(fs.called("rename_replace"), "best-effort still publishes");
        assert!(fs.exists("/dst"));
    }

    #[test]
    fn a_full_disk_does_not_publish_and_leaves_no_temporary() {
        let fs = FaultFs::new();
        fs.write_file("/src", b"hello");
        fs.fail("create_new", flux_fs::Code::DiskFull);

        let err = copy_file(&fs, Path::new("/src"), Path::new("/dst"), &opts()).unwrap_err();

        assert_eq!(err.code, flux_fs::Code::DiskFull);
        assert!(!fs.called("rename_replace"));
        assert!(!fs.exists("/dst.flux-partial.op1"));
    }

    #[test]
    fn a_failed_publish_reports_copy_failed_and_removes_the_temporary() {
        let fs = FaultFs::new();
        fs.write_file("/src", b"hello");
        fs.fail("rename_replace", flux_fs::Code::PermissionDenied);

        let err = copy_file(&fs, Path::new("/src"), Path::new("/dst"), &opts()).unwrap_err();

        assert_eq!(err.code, flux_fs::Code::CopyFailed);
        assert!(!fs.exists("/dst.flux-partial.op1"));
    }

    #[test]
    fn a_missing_source_creates_nothing() {
        let fs = FaultFs::new();
        let err = copy_file(&fs, Path::new("/nope"), Path::new("/dst"), &opts()).unwrap_err();
        assert!(!fs.called("create_new"), "nothing is created before the source is checked");
        assert_eq!(err.code, flux_fs::Code::IoError);
    }

    #[test]
    fn a_self_copy_is_refused_before_anything_is_touched() {
        let fs = FaultFs::new();
        fs.write_file("/a", b"hello");
        let err = copy_file(&fs, Path::new("/a"), Path::new("/a"), &opts()).unwrap_err();
        assert_eq!(err.code, flux_fs::Code::SafetyRejected);
        assert!(fs.calls().is_empty(), "refuse before any call; got {:?}", fs.calls());
    }

    #[test]
    fn a_source_that_changes_mid_copy_is_not_published() {
        // Section 33: the result is not published.
        let fs = FaultFs::new();
        fs.write_file("/src", b"hello");
        // The fake re-reads metadata at step 6; growing the file between the two
        // reads is what a real concurrent writer does.
        fs.grow_on_second_metadata("/src", b" world");

        let err = copy_file(&fs, Path::new("/src"), Path::new("/dst"), &opts()).unwrap_err();

        assert_eq!(err.code, flux_fs::Code::SourceChanged);
        assert!(!fs.called("rename_replace"), "must not publish a stale copy");
        assert!(!fs.exists("/dst.flux-partial.op1"));
    }

    #[test]
    fn a_zero_byte_source_copies_and_still_gets_metadata() {
        // Named in TODO.md's integration-test item, so it gets a test of its own.
        let fs = FaultFs::new();
        fs.write_file("/src", b"");
        let out = copy_file(&fs, Path::new("/src"), Path::new("/dst"), &opts()).unwrap();

        assert_eq!(out.bytes_copied, 0);
        assert!(fs.called("set_times"), "metadata still applies to an empty file");
        assert!(fs.called("rename_replace"));
    }

    #[test]
    fn no_replace_refuses_an_existing_target_and_cleans_up() {
        let fs = FaultFs::new();
        fs.write_file("/src", b"hello");
        fs.write_file("/dst", b"existing");

        let mut o = opts();
        o.publish = Publish::NoReplace;
        let err = copy_file(&fs, Path::new("/src"), Path::new("/dst"), &o).unwrap_err();

        assert_eq!(err.code, flux_fs::Code::CopyFailed);
        assert!(!fs.exists("/dst.flux-partial.op1"), "temporary must be removed");
    }

    #[test]
    fn two_default_failures_produce_two_entries_and_still_publish() {
        let fs = FaultFs::new();
        fs.write_file("/src", b"hello");
        fs.fail("set_times", flux_fs::Code::PermissionDenied);
        fs.fail("set_permissions", flux_fs::Code::PermissionDenied);

        let out = copy_file(&fs, Path::new("/src"), Path::new("/dst"), &opts()).unwrap();

        assert_eq!(out.metadata_failures.len(), 2);
        assert!(fs.called("rename_replace"));
    }

    #[test]
    fn strict_durability_failure_is_its_own_code() {
        let fs = FaultFs::new();
        fs.write_file("/src", b"hello");
        fs.fail("sync_all", flux_fs::Code::IoError);

        let mut o = opts();
        o.durability = Durability::Strict;
        let err = copy_file(&fs, Path::new("/src"), Path::new("/dst"), &o).unwrap_err();

        assert_eq!(err.code, flux_fs::Code::StrictDurabilityUnavailable);
        assert!(!fs.called("rename_replace"));
    }
```

- [ ] **Step 2: Run the tests**

Run: `cargo nextest run -p flux-core --no-tests=pass`
Expected: PASS, 16 tests. If `a_strict_metadata_failure_prevents_publication` fails, the ordering in
`copy_file` is wrong — fix the implementation, not the test.

- [ ] **Step 3: Commit**

```bash
git add crates/flux-core
git commit -m "test(flux-core): the strict/best-effort rules, proven by injection"
```

---

## Task 7: `StdFileSystem`

**Files:**
- Create: `crates/flux-platform/src/std_fs.rs`
- Modify: `crates/flux-platform/src/lib.rs`, `crates/flux-platform/Cargo.toml`

- [ ] **Step 1: Add the dependency**

In `crates/flux-platform/Cargo.toml`, before `[dev-dependencies]`:

```toml
[dependencies]
flux-fs = { path = "../flux-fs" }
```

- [ ] **Step 2: Write the failing test**

Create `crates/flux-platform/tests/std_fs.rs`:

```rust
use flux_fs::{FileSystem, Perms};
use flux_platform::StdFileSystem;
use std::io::Write;
use tempfile::TempDir;

#[test]
fn create_new_is_exclusive() {
    let d = TempDir::new().unwrap();
    let p = d.path().join("a");
    let fs = StdFileSystem;
    fs.create_new(&p).unwrap();
    assert!(fs.create_new(&p).is_err(), "FS-1: a second exclusive create must fail");
}

#[test]
fn rename_replace_replaces_an_existing_target() {
    let d = TempDir::new().unwrap();
    let (from, to) = (d.path().join("from"), d.path().join("to"));
    let fs = StdFileSystem;
    let mut f = fs.create_new(&from).unwrap();
    f.write_all(b"new").unwrap();
    drop(f);
    std::fs::write(&to, b"old").unwrap();

    fs.rename_replace(&from, &to).unwrap();
    assert_eq!(std::fs::read(&to).unwrap(), b"new");
}

#[test]
fn rename_no_replace_refuses_an_existing_target() {
    let d = TempDir::new().unwrap();
    let (from, to) = (d.path().join("from"), d.path().join("to"));
    let fs = StdFileSystem;
    fs.create_new(&from).unwrap();
    std::fs::write(&to, b"old").unwrap();

    assert!(fs.rename_no_replace(&from, &to).is_err(), "FS-2");
    assert_eq!(std::fs::read(&to).unwrap(), b"old");
}

#[test]
fn set_times_on_a_handle_moves_the_mtime() {
    let d = TempDir::new().unwrap();
    let p = d.path().join("a");
    let fs = StdFileSystem;
    let h = fs.create_new(&p).unwrap();

    let t = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000_000);
    fs.set_times(&h, Some(t)).unwrap();
    drop(h);

    let got = std::fs::metadata(&p).unwrap().modified().unwrap();
    assert_eq!(got, t);
}

#[test]
fn metadata_reports_a_directory_as_not_a_file() {
    let d = TempDir::new().unwrap();
    let fs = StdFileSystem;
    assert!(!fs.metadata(d.path()).unwrap().is_file);
}
```

- [ ] **Step 3: Run it and watch it fail**

Run: `cargo nextest run -p flux-platform --no-tests=pass`
Expected: FAIL — `unresolved import flux_platform::StdFileSystem`.

- [ ] **Step 4: Write the implementation**

Create `crates/flux-platform/src/std_fs.rs`:

```rust
//! The one real `FileSystem`, over `std::fs`.
//!
//! `rename_replace` uses `std::fs::rename`, which has POSIX replace semantics on
//! every platform Rust supports. On Windows this succeeds where
//! `MoveFileExW(MOVEFILE_REPLACE_EXISTING)` fails — FS-6 and FS-7 measured exactly
//! that, and Section 241.5 is amended in Task 9 to name it.

use flux_fs::{FileHandle, FileSystem, FsError, Metadata, Perms, Result};
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;
use std::time::SystemTime;

pub struct StdFile(File);

impl Read for StdFile {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.0.read(buf)
    }
}

impl Write for StdFile {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }
}

impl FileHandle for StdFile {
    fn sync_all(&self) -> Result<()> {
        self.0.sync_all().map_err(FsError::from_io)
    }
}

pub struct StdFileSystem;

impl FileSystem for StdFileSystem {
    type File = StdFile;

    fn open_read(&self, path: &Path) -> Result<Self::File> {
        File::open(path).map(StdFile).map_err(FsError::from_io)
    }

    fn create_new(&self, path: &Path) -> Result<Self::File> {
        OpenOptions::new()
            .write(true)
            .read(true)
            .create_new(true)
            .open(path)
            .map(StdFile)
            .map_err(FsError::from_io)
    }

    fn metadata(&self, path: &Path) -> Result<Metadata> {
        let m = std::fs::metadata(path).map_err(FsError::from_io)?;
        Ok(Metadata {
            len: m.len(),
            is_file: m.is_file(),
            permissions: Some(perms_of(&m)),
            modified: m.modified().ok(),
        })
    }

    fn set_times(&self, file: &Self::File, modified: Option<SystemTime>) -> Result<()> {
        let Some(t) = modified else { return Ok(()) };
        let times = std::fs::FileTimes::new().set_modified(t);
        file.0.set_times(times).map_err(FsError::from_io)
    }

    fn set_permissions(&self, file: &Self::File, perms: Option<Perms>) -> Result<()> {
        let Some(p) = perms else { return Ok(()) };
        let native = match p {
            #[cfg(unix)]
            Perms::UnixMode(mode) => std::os::unix::fs::PermissionsExt::from_mode(mode),
            #[cfg(not(unix))]
            Perms::UnixMode(_) => return Ok(()),
            Perms::ReadOnly(ro) => {
                let mut q = file.0.metadata().map_err(FsError::from_io)?.permissions();
                q.set_readonly(ro);
                q
            }
        };
        file.0.set_permissions(native).map_err(FsError::from_io)
    }

    fn rename_replace(&self, from: &Path, to: &Path) -> Result<()> {
        std::fs::rename(from, to).map_err(FsError::from_io)
    }

    fn rename_no_replace(&self, from: &Path, to: &Path) -> Result<()> {
        if to.exists() {
            return Err(FsError::new(
                flux_fs::Code::IoError,
                std::io::Error::from(std::io::ErrorKind::AlreadyExists),
            ));
        }
        std::fs::rename(from, to).map_err(FsError::from_io)
    }

    fn remove_file(&self, path: &Path) -> Result<()> {
        std::fs::remove_file(path).map_err(FsError::from_io)
    }
}

#[cfg(unix)]
fn perms_of(m: &std::fs::Metadata) -> Perms {
    use std::os::unix::fs::PermissionsExt;
    Perms::UnixMode(m.permissions().mode())
}

#[cfg(not(unix))]
fn perms_of(m: &std::fs::Metadata) -> Perms {
    Perms::ReadOnly(m.permissions().readonly())
}
```

Replace `crates/flux-platform/src/lib.rs`:

```rust
//! Linux, macOS and Windows implementations of the `flux-fs` abstractions (spec §3.3).

pub mod std_fs;

pub use std_fs::{StdFile, StdFileSystem};
```

`rename_no_replace`'s `to.exists()` check is a **race**, not a primitive — it is the portable
placeholder. Task 10 records that as a known limitation; `renameat2(RENAME_NOREPLACE)` and
`MoveFileExW` without `MOVEFILE_REPLACE_EXISTING` are the real primitives and arrive with the lock
protocol, which is the consumer that needs them to be atomic.

- [ ] **Step 5: Run the tests**

Run: `cargo nextest run -p flux-platform --no-tests=pass`
Expected: PASS — 5 new tests plus the 11 existing `fs_semantics` probes.

- [ ] **Step 6: Commit**

```bash
git add crates/flux-platform
git commit -m "feat(flux-platform): StdFileSystem over std::fs"
```

---

## Task 8: Wire `flux copy`

**Files:**
- Modify: `crates/flux-cli/src/main.rs`, `crates/flux-cli/Cargo.toml`

- [ ] **Step 1: Add the dependencies**

In `crates/flux-cli/Cargo.toml`, after the `[[bin]]` block:

```toml
[dependencies]
clap = { workspace = true }
flux-core = { path = "../flux-core" }
flux-fs = { path = "../flux-fs" }
flux-platform = { path = "../flux-platform" }
```

- [ ] **Step 2: Write the failing test**

Create `crates/flux-cli/tests/copy.rs`:

```rust
use std::process::Command;
use tempfile::TempDir;

fn flux() -> Command {
    Command::new(env!("CARGO_BIN_EXE_flux"))
}

#[test]
fn it_copies_a_single_file() {
    let d = TempDir::new().unwrap();
    let (src, dst) = (d.path().join("a"), d.path().join("b"));
    std::fs::write(&src, b"hello").unwrap();

    let out = flux().arg("copy").arg(&src).arg(&dst).output().unwrap();

    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(std::fs::read(&dst).unwrap(), b"hello");
}

#[test]
fn it_leaves_no_temporary_behind() {
    let d = TempDir::new().unwrap();
    let (src, dst) = (d.path().join("a"), d.path().join("b"));
    std::fs::write(&src, b"hello").unwrap();

    flux().arg("copy").arg(&src).arg(&dst).output().unwrap();

    let leftovers: Vec<_> = std::fs::read_dir(d.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().contains("flux-partial"))
        .collect();
    assert!(leftovers.is_empty(), "found {leftovers:?}");
}
```

Add to `crates/flux-cli/Cargo.toml`:

```toml
[dev-dependencies]
tempfile = { workspace = true }
```

- [ ] **Step 3: Run it and watch it fail**

Run: `cargo nextest run -p flux-cli --no-tests=pass`
Expected: FAIL — the binary exits 1 with "flux: not implemented yet".

- [ ] **Step 4: Write the implementation**

Replace `crates/flux-cli/src/main.rs`:

```rust
//! Flux CLI (spec §3.6): argument parsing and command dispatch.

use clap::{Parser, Subcommand};
use flux_fs::{CopyOptions, Durability, OperationId, Preserve, Publish};
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Parser)]
#[command(name = "flux", version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Copy a single file.
    Copy {
        source: PathBuf,
        destination: PathBuf,
        /// Fail the file if its timestamps cannot be applied (§44.1).
        #[arg(long)]
        preserve_times: bool,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Commands::Copy { source, destination, preserve_times } => {
            let opts = CopyOptions {
                // Explicitly requested is Strict; otherwise applied best-effort.
                preserve_times: if preserve_times { Preserve::Strict } else { Preserve::Default },
                preserve_permissions: Preserve::Default,
                durability: Durability::Normal,
                publish: Publish::Replace,
                operation_id: OperationId::new(format!("{}", std::process::id())),
            };
            match flux_core::copy_file(&flux_platform::StdFileSystem, &source, &destination, &opts) {
                Ok(outcome) => {
                    for f in &outcome.metadata_failures {
                        eprintln!("METADATA_APPLY_FAILED: {:?}: {}", f.item, f.error);
                    }
                    // §44.1: best-effort failures still publish, but the operation exits 1.
                    if outcome.metadata_failures.is_empty() {
                        ExitCode::SUCCESS
                    } else {
                        ExitCode::from(1)
                    }
                }
                Err(e) => {
                    eprintln!("{}: {}", e.code.as_str(), e.source);
                    ExitCode::from(1)
                }
            }
        }
    }
}
```

**`OperationId` uses the process id.** §18.1 wants collision-resistant and deterministic enough for
discovery; a pid is deterministic within a run and unique among live processes, which is what this
cut needs. It is **not** sufficient for cross-run discovery, and the spec document says so.

- [ ] **Step 5: Run the tests**

Run: `cargo nextest run -p flux-cli --no-tests=pass`
Expected: PASS, 2 tests.

- [ ] **Step 6: Run the whole gate**

Run: `just check`
Expected: fmt, clippy, typos and the full test suite all pass.

- [ ] **Step 7: Commit**

```bash
git add crates/flux-cli
git commit -m "feat(flux-cli): wire flux copy to the engine"
```

---

## Task 9: Amend §241.5 and close the TODO item

**Files:**
- Modify: `FLUX_FULL_UPDATED_SPEC_V16.md`, `TODO.md`, `models/lockproto/spec-sections.stamp`

- [ ] **Step 1: Find the section**

```bash
grep -n "241\.5" FLUX_FULL_UPDATED_SPEC_V16.md | head -5
grep -n "^## 241" FLUX_FULL_UPDATED_SPEC_V16.md
```

Read what is there before editing. If §241.5 already names a replacing-rename API, **stop** and report
`STATE_MISMATCH` — the TODO item would already be closed and this task is void.

- [ ] **Step 2: Add the sentence**

In §241.5, after the existing `MoveFileEx` sentence for no-replace publication:

```markdown
The replacing publication uses the platform's POSIX-semantics rename —
`renameat2` without `RENAME_NOREPLACE` on Linux, `renamex_np` on macOS, and on
Windows `SetFileInformationByHandle` with `FileRenameInfoEx` and
`FILE_RENAME_FLAG_POSIX_SEMANTICS`, which is what `std::fs::rename` issues.
`MoveFileExW(MOVEFILE_REPLACE_EXISTING)` is **not** usable for it: it fails with
`ERROR_ACCESS_DENIED` whenever the target is open, which a concurrent reader makes
routine. Measured: `crates/flux-platform/tests/fs_semantics.rs`,
`fs6_file_open_without_delete_sharing_cannot_be_renamed_deleted_or_replaced` and
`fs7_file_open_with_delete_sharing_can_be_renamed_deleted_and_replaced`.
```

- [ ] **Step 3: Re-stamp if the spec-drift test requires it**

Run: `cargo nextest run --test model_stamp --no-tests=pass`

If it fails reporting a changed unit hash, that is the stamp doing its job. Follow the failure's own
instructions to re-stamp; do **not** edit `spec-sections.stamp` by hand.

- [ ] **Step 4: Close the TODO item**

In `TODO.md`, remove the "Windows replacing rename is unnamed" item from **Spec gaps from the
filesystem probes** and add to `## Closed`:

```markdown
- `DONE` **Windows replacing rename is unnamed.** §241.5 now names the POSIX-semantics rename for
  replacing publication and states why `MoveFileExW(MOVEFILE_REPLACE_EXISTING)` cannot serve, citing
  the FS-6 and FS-7 probes. Implemented as `StdFileSystem::rename_replace`.
```

- [ ] **Step 5: Verify the count**

```bash
grep -c "^- \[ \]" TODO.md
grep -c "^- \`" TODO.md
```

Expected: 47 open and 3 closed. They must sum to 50 — the 49 triaged plus the release-PR-guard item
added afterwards.

- [ ] **Step 6: Commit**

```bash
git add FLUX_FULL_UPDATED_SPEC_V16.md TODO.md models/lockproto/
git commit -m "spec: name the replacing-rename API for publication (§241.5)"
```

---

## Task 10: Record what this cut does not do

**Files:**
- Modify: `.clavity/local-anomalies.md`

- [ ] **Step 1: Append the two limitations**

```markdown
## 2026-09-22 — two known limits of the first single-file copy

### `rename_no_replace` is a check-then-rename race

`StdFileSystem::rename_no_replace` tests `to.exists()` and then renames. Between the two, another
process can create the target, and the rename replaces it — exactly what the method promises not to do.
The portable primitives are `renameat2(RENAME_NOREPLACE)` on Linux, `renamex_np(RENAME_EXCL)` on macOS
and `MoveFileExW` without `MOVEFILE_REPLACE_EXISTING` on Windows.

Not fixed now because single-file copy publishes with `Publish::Replace`; the consumer that needs an
atomic no-replace is the lock protocol, which is where the primitive belongs.

### A leftover temporary from a PREVIOUS run is never removed

`<target>.flux-partial.<operation-id>` is named with a per-invocation id (the pid), so step 0 only ever
removes this invocation's own leftover. §18.1 wants an id "deterministic enough for discovery"; that
needs operation state to record it, which this cut does not have. A crashed run therefore leaves a
temporary that nothing collects.
```

- [ ] **Step 2: Commit**

```bash
git add .clavity/local-anomalies.md
git commit -m "docs: record the two known limits of the first copy implementation"
```

Note: `.clavity/` is gitignored, so this commit may stage nothing. If `git status` shows no change,
say so and move on — the entry still exists locally for the next triage.

---

## Task 11: Push and open the pull request

- [ ] **Step 1: Run the full gate one more time**

```bash
just check
```

- [ ] **Step 2: Push and open**

```bash
git push -u origin <branch>
gh pr create --base main --title "feat: flux-fs trait surface and single-file copy"
```

Body: what now works (`flux copy` copies a file), the §44.1 ordering and the test that proves it, the
spec amendment, and the two limitations from Task 10.
