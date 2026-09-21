# `flux-fs` trait surface and single-file copy — design

**Date:** 2026-09-22
**Status:** approved, not yet planned
**Scope:** the portable filesystem trait, and single-file copy with metadata preservation. The first
product code in this repository.

## Why now

`flux-cli` prints `flux: not implemented yet` and exits 1. `flux-fs/src/lib.rs` is a three-line doc
comment. Nothing in the product is implemented.

The gate on starting was `TODO.md`'s "Model-check the lock protocol **before** implementing it", and
that gate is met: lock-model plans 1 to 3 modelled two recoverers, two `--break-lock` takeovers, a
stalled prior owner and a plain run, with `SingleWriter` checked across 43 configurations. It was
closed in the triage of 2026-09-22.

Recursive directory copy is **out of scope**: it is blocked on the directory-walker decision, which is
still open.

## Decisions taken before the design

1. **The trait covers only what single-file copy needs.** It grows when a second consumer needs
   something. Every method has a caller and a test from day one.
2. **Bytes move through a portable read/write fallback.** No `copy_file_range`, no reflink, no
   `CopyFileExW`. §530 requires every platform optimization to have a safe fallback, so this code is
   needed whatever is added later.
3. **All three platforms**, Linux, macOS and Windows.
4. **The trait is synchronous.** Not a preference — the spec's candidate crates are `rayon` and
   `crossbeam-channel`, the workspace pins both, `--workers <N|auto>` exists, and no async runtime
   appears anywhere in the tree. Blocking I/O on a worker pool.

## The constraint that shapes everything

§2569:

> Metadata the user explicitly requested (`--preserve`, `--preserve-times`,
> `--preserve-permissions`) is strict. If it cannot be applied to a file, that file's action fails
> with `METADATA_APPLY_FAILED`, and under atomic replacement the file is not published.
> Metadata applied only by default is best-effort. The file is published, each failure is reported as
> `METADATA_APPLY_FAILED`, and the operation exits with status 1.

**Metadata must therefore be applied to the temporary, before publication.** An implementation that
copies, publishes, then applies metadata has already violated the spec, and no amount of error
handling repairs it.

## The trait

```rust
pub trait FileSystem: Send + Sync {
    type File: FileHandle;

    fn open_read(&self, path: &Path) -> Result<Self::File>;
    fn create_new(&self, path: &Path) -> Result<Self::File>;      // exclusive; FS-1
    fn metadata(&self, path: &Path) -> Result<Metadata>;
    fn set_times(&self, file: &Self::File, times: FileTimes) -> Result<()>;
    fn set_permissions(&self, file: &Self::File, perms: Permissions) -> Result<()>;
    fn rename_replace(&self, from: &Path, to: &Path) -> Result<()>;
    fn rename_no_replace(&self, from: &Path, to: &Path) -> Result<()>;   // FS-2
    fn remove_file(&self, path: &Path) -> Result<()>;
}

pub trait FileHandle: Read + Write {
    fn sync_all(&self) -> Result<()>;
}
```

Three choices, each backed by a measured probe rather than by taste:

- **Metadata is set on a handle, not a path.** The temporary must be fully prepared before
  publication, and addressing it by path in between is a race: something else can take that name.
  FS-4 and FS-5 measured that a handle survives both rename and unlink, so the handle is the stable
  referent. This is what makes strict metadata enforceable at all.
- **`create_new` is exclusive.** FS-1 measured that a second exclusive create fails, which is what
  makes `<target>.flux-partial.<operation-id>` safe.
- **Two rename methods rather than one with a flag.** §1286 step 2 publishes by rename "refusing to
  replace when the target was planned as new"; those are different operations with different failure
  modes, and FS-2 measured the no-replace case separately. A boolean hides that at the call site.

`Send + Sync` because rayon workers share one instance.

### Why a primitives trait rather than a whole-operation trait

Considered and rejected: a single `copy_file(&self, src, dst, opts)` implemented per platform, and
concrete `std::fs` code with no trait at all.

- A whole-operation trait triplicates the one piece of logic the spec is fussiest about — the
  ordering above — and guarantees it drifts. It is also throwaway: the next consumer is a lock
  protocol needing sharing modes, file identity and rename semantics, for which `copy_file` is
  useless.
- Concrete code with no trait makes the failure paths untestable, and failure handling is most of
  what makes a copy tool trustworthy. It also walks into FS-6, FS-7 and FS-8: Windows sharing
  behaviour and file identity are not uniformly exposed by `std::fs`, so the trait gets retrofitted
  later, into `flux-core`, while the lock protocol is being built.

This matches the independent recommendation of the agy consult (2026-09-22), which reached the same
shape from the throwaway-cost argument rather than from testability.

## Types and contracts

Named here because the audit found the trait signatures leaning on undefined names.

```rust
pub type Result<T> = std::result::Result<T, FsError>;

/// Only what single-file copy reads. Widened when a consumer needs more.
pub struct Metadata {
    pub len: u64,
    pub is_file: bool,          // false for dirs, symlinks, devices, FIFOs
    pub permissions: Permissions,
    pub times: FileTimes,
}

/// std::fs::Permissions and std::fs::FileTimes are re-exported, not re-invented.
pub use std::fs::{FileTimes, Permissions};

pub struct CopyOptions {
    pub preserve_times: Preserve,       // Strict | Default | Off
    pub preserve_permissions: Preserve,
    pub durability: Durability,         // Normal | Strict
    pub publish: Publish,               // Replace | NoReplace
    pub operation_id: OperationId,
}

pub enum Preserve { Strict, Default, Off }

pub struct Outcome {
    pub bytes_copied: u64,
    pub metadata_failures: Vec<MetadataFailure>,   // empty on a clean copy
}

pub struct MetadataFailure {
    pub item: MetadataItem,    // Times | Permissions
    pub error: FsError,
}
```

**`Preserve` has three states, not two.** `Strict` is explicitly requested and its failure prevents
publication; `Default` is applied best-effort; `Off` is not attempted at all. Collapsing `Default` and
`Off` into a boolean would make "not requested" and "requested leniently" indistinguishable, and §2569
treats them differently.

**`OperationId`** is a value generated once per `flux` invocation and threaded through `CopyOptions`.
It exists solely to name the temporary, per §1286 step 0, so a crashed run's leftover is identifiable
as its own. There is no operation *state* in this cut — no manifest, no `state.db` — and nothing here
persists the id. A newtype over a UUID or a timestamp-plus-pid is sufficient; the plan picks one.

**`FsError`** wraps `std::io::Error` and carries the spec code from the taxonomy below, so a caller can
both match on the code and report the underlying OS message. The platform layer maps `ENOSPC` and
`ERROR_DISK_FULL` to `DiskFull`, `EACCES` and `ERROR_ACCESS_DENIED` to `PermissionDenied`, and
anything unrecognised to `IoError` — the mapping is the platform layer's job, because the raw codes
differ per OS and `flux-core` must not branch on them.

## The copy algorithm

Lives once, in `flux-core`, generic over the trait.

```
copy_file(fs, src, dst, opts) -> Result<Outcome>

 0. remove any leftover <dst>.flux-partial.<operation-id>          §1286 step 0
 1. open_read(src); metadata(src)                                  capture for the step-6 re-check
 2. create_new(<dst>.flux-partial.<operation-id>)                  exclusive, FS-1
 3. stream bytes src -> temp
 4. sync_all(temp)                                                 if --durability=strict
 5. apply metadata to the TEMP HANDLE
       strict item fails  -> remove temp; METADATA_APPLY_FAILED; DO NOT PUBLISH
       default item fails -> record in Outcome.metadata_failures; continue
 6. re-check source metadata; if changed -> SOURCE_CHANGED, do not publish   §33
    publish: rename_replace, or rename_no_replace when planned as new        §1286 step 2
 7. remove temp if it still exists                                 §1286 step 3
 8. Outcome { bytes, metadata_failures }
```

**Step 5 precedes step 6, and that is the design.** See the constraint above.

**Best-effort is not an error path.** A default metadata failure publishes, returns `Ok`, and carries
the failure in `Outcome`. The caller reports each as `METADATA_APPLY_FAILED` and the *operation* exits
1. A single `Result` cannot express "succeeded with complaints", which is why `Outcome` carries them.

**Cleanup is step 0's job, not a destructor.** The temporary's name carries this operation's id, so a
crashed run's leftover is unambiguously its own to remove on the next attempt. No `Drop` guard, no
panic-safety machinery, no cleanup path that can itself fail mid-unwind.

**`SOURCE_CHANGED` is a second pre-publication gate.** §33 says the result is not published when
source identity or metadata changed during the copy. It needs the step-1 capture and the step-6
re-check. Included now rather than later, because omitting it means knowingly publishing a stale file,
and adding it later means touching the publish path again.

## Out of scope, named so nobody assumes otherwise

- **`--atomic=auto`'s in-place fallback.** §1900 permits it when capacity makes atomic infeasible, and
  the decision must be reported. That needs capacity estimation and a reporting channel, neither of
  which exists. This cut implements the atomic path and errors when it cannot be used.
- **Resume, verification, sparseness, reflinks, hardlinks, special files.** Each has its own spec
  section and its own Phase-2+ item.
- **Recursive copy**, blocked on the directory-walker decision.

## Cases the algorithm must answer

Enumerated because the audit found the happy path specified and these not.

| case | behaviour |
|---|---|
| `dst`'s parent directory does not exist | `create_new` fails; return `IO_ERROR`. Creating parents is the recursive-copy item's job, not this one. |
| `src` is a symlink, device, FIFO or socket | `Metadata.is_file` is false: refuse with `SPECIAL_FILE_UNSUPPORTED` before opening anything. §233 defines `--special-files=<skip\|strict>`; this cut implements neither mode and refuses, which is the conservative reading. |
| `src` is a directory | same path: `is_file` is false, refused. |
| `dst` exists and is a directory | `rename_replace` fails at step 6; the temporary is removed and `IO_ERROR` returned. Directory replacement is §87's business. |
| `src` and `dst` resolve to the same file | refuse with `SAFETY_REJECTED` before step 2. A self-copy would otherwise truncate the source into its own temporary. |
| `src` is zero bytes | copies normally; step 3 writes nothing and step 5 still applies metadata. Named because `TODO.md`'s integration-test item lists zero-byte files explicitly. |
| `dst` exists and `publish` is `NoReplace` | `rename_no_replace` fails at step 6; temporary removed. FS-2 measured this failure. |
| a leftover temporary exists from **another** operation id | left alone. Step 0 removes only this operation's own leftover, per §1286. |
| the copy is interrupted (crash, kill) | the temporary survives; the next run with the same operation id removes it at step 0. Nothing else is needed. |
| `set_times` succeeds and `set_permissions` fails, both strict | abort on the first strict failure; do not attempt the rest. The file is not published, so the partial metadata is discarded with the temporary. |
| both `Default`, both fail | published, `metadata_failures` has two entries, `Ok`. |

## Error taxonomy

The spec defines the codes; this cut implements the subset single-file copy can actually produce.

| code | produced when |
|---|---|
| `COPY_FAILED` | the content copy failed |
| `METADATA_APPLY_FAILED` | strict: action fails, unpublished. default: published, reported, exit 1 |
| `DISK_FULL` | `ENOSPC` / `ERROR_DISK_FULL` on a destination allocation |
| `PERMISSION_DENIED` | the operating system denied access |
| `SOURCE_CHANGED` | source identity or metadata changed during the copy; not published |
| `STRICT_DURABILITY_UNAVAILABLE` | `--durability=strict` cannot be established |
| `IO_ERROR` | anything not covered above, and it stays a catch-all |

**The error enum carries only variants with a reachable producer.** An unreachable variant is a design
error, not dead code to be tolerated. `REFLINK_UNAVAILABLE`, `SPARSE_UNAVAILABLE` and the lock codes
belong to features this cut does not build.

## Testing

Two tiers, proving different things.

### Fault injection against the trait, in `flux-core`

Where the spec's rules are proven. A `FaultFs` fails the Nth call to a chosen primitive. Each test is
named for the rule it pins and asserts on **which primitives were called, and in what order** —
because the ordering is the requirement.

| test | asserts |
|---|---|
| strict `set_times` fails | temp removed, `MetadataApplyFailed`, **`rename_replace` never called** |
| default `set_times` fails | `rename_replace` **is** called, one `metadata_failures` entry, `Ok` |
| `write` returns `ENOSPC` partway | `DISK_FULL`, no publish, no leftover temp |
| `rename_replace` fails | `COPY_FAILED`, temp removed |
| source metadata differs at step 6 | `SOURCE_CHANGED`, no publish |
| leftover temp from a prior run | removed at step 0, copy proceeds |

Deterministic, no disk, no timing.

### Real filesystem, in `flux-platform`

Only what a fake cannot tell you: that `set_times` on a handle moves the mtime, that `create_new` is
genuinely exclusive, that `rename_replace` replaces on all three platforms. These extend the existing
`crates/flux-platform/tests/fs_semantics.rs` probes rather than starting a parallel suite.

**A fake filesystem can prove the algorithm; only a real one can prove the primitives.** Testing the
algorithm against a real disk is slow, flaky, and still misses the interesting cases; testing the
primitives against a fake proves nothing.

## A spec gap this work closes

`TODO.md` carries: *"Windows replacing rename is unnamed. `std::fs::rename` replaces a target that is
open with delete-sharing, but `MoveFileExW(MOVEFILE_REPLACE_EXISTING)` fails with 'Access is denied'
whenever the target is open. §241.5 names `MoveFileEx` for the no-replace publish only."*

Implementing `rename_replace` on Windows forces the choice. The evidence is already measured: FS-6 and
FS-7 show `MoveFileExW` failing where `std::fs::rename` succeeds. This design adopts `std::fs::rename`
as the replacing-publish primitive and the implementation amends §241.5 to name it, closing the TODO
item with the probe as its citation.

## Acceptance

- `flux copy <src> <dst>` copies a single file with metadata, and no longer exits 1 unconditionally.
- Every fault-injection test above passes, including the two that assert a publish did **not** happen.
- The real-filesystem tests pass on Linux, macOS and Windows in CI.
- §241.5 names the replacing-rename API, and the `TODO.md` item is closed with its measurement.
- No error variant exists without a test that produces it.
