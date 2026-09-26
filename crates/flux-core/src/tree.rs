//! Recursive copy (cut 4b): `copy_tree` drives the ordered walk and `copy_file_at`,
//! writing only through destination directory handles (§149.7).
//!
//! Design authority: `docs/superpowers/specs/2026-09-26-cut-4b-copy-tree-design.md`.

use crate::copy::{CopyError, CopyStep, copy_file_at, split_destination, weaker};
use crate::walk::{WalkEvent, walk};
use flux_fs::{
    Code, CopyOptions, DestinationRoot, DirHandle, FileIdentity, FileType, FsError,
    MetadataFailure, ObjectId, Publish, Safety,
};
use std::collections::{BTreeMap, HashSet};
use std::io::ErrorKind;
use std::path::{Component, Path, PathBuf};

/// What a tree copy did, counted. Individual failures are STREAMED to the sink as
/// they happen, never accumulated here, so a tree with a million failures costs what a
/// clean one does.
#[derive(Debug, Default)]
pub struct TreeOutcome {
    pub files_copied: u64,
    pub bytes_copied: u64,
    /// Directories THIS operation created, the root included when it made it. A
    /// pre-existing directory merged into is not counted.
    pub directories_created: u64,
    pub failures: FailureTally,
    /// Identity comparisons that were SKIPPED because a side was not `Strong`. Not
    /// failures: the copies happened. The engine captures; cut 5's CLI renders.
    pub warnings: WeakIdentityWarnings,
}

#[derive(Debug)]
pub struct TreeFailure {
    /// Relative to the source root, as the walk reports it.
    pub path: PathBuf,
    pub cause: TreeFailureCause,
}

/// Each variant is defined by WHERE the failure happened, so no failure can be
/// expressed two ways.
#[derive(Debug)]
pub enum TreeFailureCause {
    /// The walk could not read or enter something.
    Walk(FsError),
    /// A destination directory could not be created or entered - including a
    /// `DESTINATION_NAMESPACE_COLLISION` between two source names the destination folds
    /// into one. Its subtree is skipped and reported once, here.
    CreateDir(FsError),
    /// One file's copy failed. `CopyError` intact, so a leftover temporary is still
    /// reported per file.
    Copy(CopyError),
    /// A symlink or other non-regular entry, which this cut does not recreate.
    Unsupported(FileType),
    /// The file IS at the destination; some of its metadata could not be applied.
    /// Published with complaints, which exits 1 like the single-file case.
    PublishedWithComplaints(Vec<MetadataFailure>),
}

/// One counter per `TreeFailureCause` variant. Fixed size: it costs the same on a
/// clean tree and on one where every entry failed.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct FailureTally {
    pub walk: u64,
    pub create_dir: u64,
    pub copy: u64,
    pub unsupported: u64,
    pub published_with_complaints: u64,
}

impl FailureTally {
    /// The only total. Derived, never stored beside the parts.
    pub fn total(&self) -> u64 {
        self.walk + self.create_dir + self.copy + self.unsupported + self.published_with_complaints
    }

    pub fn is_empty(&self) -> bool {
        self.total() == 0
    }

    /// EXHAUSTIVE, with no wildcard arm: a new cause must name its counter here or the
    /// crate does not compile. A `_ =>` arm would let a new cause count nothing, and the
    /// miscount would surface only as a wrong exit code.
    pub(crate) fn count(&mut self, cause: &TreeFailureCause) {
        match cause {
            TreeFailureCause::Walk(_) => self.walk += 1,
            TreeFailureCause::CreateDir(_) => self.create_dir += 1,
            TreeFailureCause::Copy(_) => self.copy += 1,
            TreeFailureCause::Unsupported(_) => self.unsupported += 1,
            TreeFailureCause::PublishedWithComplaints(_) => self.published_with_complaints += 1,
        }
    }
}

/// Skipped identity comparisons, aggregated for one warning apiece (walker design,
/// "When identity is weak or unavailable").
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct WeakIdentityWarnings {
    /// One group per affected volume (`ObjectId::volume`), in stable order.
    pub weak: BTreeMap<u64, DegradedGroup>,
    /// Everything whose identity was `Unavailable`, which names no volume to group by.
    pub unavailable: Option<DegradedGroup>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DegradedGroup {
    pub count: u64,
    /// The first path that degraded, relative to the source root. The EMPTY path is the
    /// source root itself (the §129 pre-flight).
    pub example: PathBuf,
}

impl WeakIdentityWarnings {
    pub fn is_empty(&self) -> bool {
        self.weak.is_empty() && self.unavailable.is_none()
    }

    /// Record one comparison that was skipped. `weaker` is the weaker side, as
    /// `copy::weaker` computes it; `Strong` is not a degradation and is ignored.
    pub(crate) fn record(&mut self, weaker: FileIdentity, example: &Path) {
        let group = match weaker {
            FileIdentity::Strong(_) => return,
            FileIdentity::Weak(id) => self
                .weak
                .entry(id.volume)
                .or_insert_with(|| DegradedGroup { count: 0, example: example.to_path_buf() }),
            FileIdentity::Unavailable => self
                .unavailable
                .get_or_insert_with(|| DegradedGroup { count: 0, example: example.to_path_buf() }),
        };
        group.count += 1;
    }
}

/// The directory one frame of the walk writes into, or a marker that its subtree was
/// refused. One entry per `Dir` the walk emits, popped at its `DirEnd`: `walk.rs`'s
/// `enter` pushes a frame of its own before it returns `Dir` and none on `Err`, and
/// `next` emits `DirEnd` for every such frame, so this stack cannot drift from the walk.
enum Frame<D> {
    Live {
        dir: D,
        /// `Strong` identities of the directories this operation created INSIDE `dir`.
        /// A fold is two source names in ONE source directory landing on one
        /// destination name, so both creations happen here; the set is dropped with the
        /// frame, which bounds it like the walk's one-directory listing (spec line 997,
        /// invariant 11).
        created: HashSet<ObjectId>,
    },
    /// Its directory failed; everything below it is skipped, and was reported once.
    Skipped,
}

/// Copy the tree at `src_root` into `dst_root`, writing only through directory
/// handles. See the design for the full contract; in brief:
///
/// - The outer `Err` means the operation did not happen or had to stop: the source
///   root missing or not a directory; the lexical floor; the §129 pre-flight (an
///   identity match, or a degraded comparison under `Safety::Strict`); a directory
///   reached mid-walk that is the destination by identity, or whose comparison is
///   degraded under `Strict`; a destination root that cannot be resolved or created;
///   and the first publish reporting that the no-replace primitive is unavailable.
/// - Every other failure goes to `on_failure`, and the walk continues.
/// - Every file is published with `Publish::NoReplace` whatever `opts.publish` says
///   (§241.5): an existing destination file is never replaced; it is reported
///   `DESTINATION_NAMESPACE_COLLISION`.
pub fn copy_tree<F: DestinationRoot>(
    fs: &F,
    src_root: &Path,
    dst_root: &Path,
    opts: &CopyOptions,
    on_failure: &mut dyn FnMut(TreeFailure),
) -> std::result::Result<TreeOutcome, CopyError> {
    // 1. The source root. `walk` refuses a missing or non-directory root: the whole
    //    operation failing, before any destination call.
    let events = walk(fs, src_root).map_err(|e| CopyError::at(CopyStep::Source, e))?;
    let src_identity =
        fs.metadata(src_root).map_err(|e| CopyError::at(CopyStep::Source, e))?.identity;

    // 2. The lexical floor, before any destination call (§129's own example, `/data`
    //    into `/data/backup`). Runs at every identity strength.
    if lexically_within(dst_root, src_root) {
        return Err(refuse("the destination is the source or lies inside it"));
    }

    let mut out = TreeOutcome::default();

    // 3-5. Resolve the destination, compare it with the source, create the root.
    let resolve = |e| CopyError::at(CopyStep::Resolve, e);
    let root = match fs.destination_root(dst_root) {
        Ok(root) => {
            preflight(
                src_identity,
                root.identity().map_err(resolve)?,
                opts.safety,
                &mut out.warnings,
            )?;
            root
        }
        Err(e) if e.source.kind() == ErrorKind::NotFound => {
            let (parent_path, name) = split_destination(dst_root)?;
            let parent = fs.destination_root(parent_path).map_err(resolve)?;
            preflight(
                src_identity,
                parent.identity().map_err(resolve)?,
                opts.safety,
                &mut out.warnings,
            )?;
            // Decision 8 on the root, every failure fatal. Nothing else is created on
            // `parent`, so there is no sibling to fold against.
            match parent.create_dir(name) {
                Ok(root) => {
                    out.directories_created += 1;
                    root
                }
                Err(e) if e.source.kind() == ErrorKind::AlreadyExists => {
                    parent.open_dir(name).map_err(resolve)?
                }
                Err(e) => return Err(resolve(e)),
            }
        }
        Err(e) => return Err(resolve(e)),
    };
    let root_identity = root.identity().map_err(resolve)?;

    // 6. The walk. NoReplace is mandatory in a tree (§241.5): every target is planned
    //    as new, and an existing one is refused at Step 2a (F3).
    let opts = CopyOptions { publish: Publish::NoReplace, ..opts.clone() };
    let mut stack = vec![Frame::Live { dir: root, created: HashSet::new() }];
    for item in events {
        let live = matches!(stack.last(), Some(Frame::Live { .. }));
        let event = match item {
            Ok(event) => event,
            // Under a skipped subtree the walk still reads; its errors there belong to
            // a failure already reported once.
            Err(e) => {
                if live {
                    report(&mut out, on_failure, e.path, TreeFailureCause::Walk(e.cause));
                }
                continue;
            }
        };
        match event {
            WalkEvent::Dir { path, identity } => {
                let frame = if live {
                    enter_dir(
                        &mut stack,
                        &path,
                        identity,
                        root_identity,
                        &opts,
                        &mut out,
                        on_failure,
                    )?
                } else {
                    Frame::Skipped
                };
                stack.push(frame);
            }
            WalkEvent::File { path } => {
                if let Some(Frame::Live { dir, .. }) = stack.last() {
                    copy_one(fs, src_root, dir, path, &opts, &mut out, on_failure)?;
                }
            }
            WalkEvent::Symlink { path } => {
                if live {
                    report(
                        &mut out,
                        on_failure,
                        path,
                        TreeFailureCause::Unsupported(FileType::Symlink),
                    );
                }
            }
            WalkEvent::Other { path } => {
                if live {
                    report(
                        &mut out,
                        on_failure,
                        path,
                        TreeFailureCause::Unsupported(FileType::Other),
                    );
                }
            }
            WalkEvent::DirEnd { .. } => {
                stack.pop();
            }
        }
    }
    Ok(out)
}

/// A `Dir` event under a live frame: the dynamic §129 check, then decision 8.
fn enter_dir<D: DirHandle>(
    stack: &mut [Frame<D>],
    path: &Path,
    identity: FileIdentity,
    root_identity: FileIdentity,
    opts: &CopyOptions,
    out: &mut TreeOutcome,
    on_failure: &mut dyn FnMut(TreeFailure),
) -> std::result::Result<Frame<D>, CopyError> {
    // The dynamic half of §129 / §149.6: a directory reached mid-walk that IS the
    // destination root means the source reached it by an alias. Checked before
    // anything is created for it.
    match (identity, root_identity) {
        (FileIdentity::Strong(a), FileIdentity::Strong(b)) if a == b => {
            return Err(refuse("a source directory is the destination itself, by identity"));
        }
        (FileIdentity::Strong(_), FileIdentity::Strong(_)) => {}
        (s, d) => match opts.safety {
            Safety::Default => out.warnings.record(weaker(s, d), path),
            Safety::Strict => {
                return Err(refuse(
                    "a source directory cannot be compared with the destination with full confidence",
                ));
            }
        },
    }

    let Some(Frame::Live { dir: parent, created }) = stack.last_mut() else {
        unreachable!("enter_dir is called only under a live frame");
    };
    let name = path.file_name().expect("a walk path ends in a name");
    // Decision 8: create first; open on AlreadyExists. Neither call traverses a link.
    let failure = match parent.create_dir(name) {
        Ok(child) => {
            out.directories_created += 1;
            if let Ok(FileIdentity::Strong(id)) = child.identity() {
                created.insert(id);
            }
            return Ok(Frame::Live { dir: child, created: HashSet::new() });
        }
        Err(e) if e.source.kind() == ErrorKind::AlreadyExists => match parent.open_dir(name) {
            Ok(child) => match child.identity() {
                // Created by THIS operation moments ago, under another source name.
                Ok(FileIdentity::Strong(id)) if created.contains(&id) => FsError::new(
                    Code::DestinationNamespaceCollision,
                    std::io::Error::other(
                        "this operation already created that directory under another source name",
                    ),
                ),
                // Pre-existing: merge into it.
                _ => return Ok(Frame::Live { dir: child, created: HashSet::new() }),
            },
            // SAFETY_REJECTED (a link) or DESTINATION_ERROR (a file, or it vanished).
            Err(e) => e,
        },
        Err(e) => e,
    };
    report(out, on_failure, path.to_path_buf(), TreeFailureCause::CreateDir(failure));
    Ok(Frame::Skipped)
}

/// A `File` event under a live frame.
fn copy_one<F: DestinationRoot>(
    fs: &F,
    src_root: &Path,
    parent: &F::Dir,
    path: PathBuf,
    opts: &CopyOptions,
    out: &mut TreeOutcome,
    on_failure: &mut dyn FnMut(TreeFailure),
) -> std::result::Result<(), CopyError> {
    let name = path.file_name().expect("a walk path ends in a name");
    match copy_file_at(fs, &src_root.join(&path), parent, name, opts) {
        Ok(o) => {
            out.files_copied += 1;
            out.bytes_copied += o.bytes_copied;
            if let Some(weaker) = o.identity_degraded {
                out.warnings.record(weaker, &path);
            }
            if !o.metadata_failures.is_empty() {
                report(
                    out,
                    on_failure,
                    path,
                    TreeFailureCause::PublishedWithComplaints(o.metadata_failures),
                );
            }
            Ok(())
        }
        Err(mut e) => {
            // `copy_file_at` records a leftover relative to its handle; rebuild it in
            // the tree's frame, relative to the destination root.
            if let Some((p, _)) = e.leftover.as_mut() {
                *p = path.with_file_name(&*p);
            }
            // Decision 3: this destination lacks a no-replace primitive. Found at the
            // FIRST publish, so the whole operation stops instead of failing every file.
            if e.step == CopyStep::Publish && primitive_unavailable(&e.cause.source) {
                e.cause.code = Code::NoReplacePublishUnavailable;
                return Err(e);
            }
            // F3 and §241.5: the name was taken - by a file that already existed
            // (refused at Step 2a) or one that appeared before publish (the no-replace
            // rename refused it). `source` is kept, so `raw_os_error` survives.
            if matches!(e.step, CopyStep::Gate | CopyStep::Publish)
                && e.cause.source.kind() == ErrorKind::AlreadyExists
            {
                e.cause.code = Code::DestinationNamespaceCollision;
            }
            report(out, on_failure, path, TreeFailureCause::Copy(e));
            Ok(())
        }
    }
}

/// "Primitive unavailable" (decision 3). `Unsupported` covers ENOSYS and EOPNOTSUPP
/// (std maps both there) and the fake's `set_no_replace_support(false)`; a RAW `EINVAL`
/// on unix is the measured WSL 9p answer. An `InvalidInput` with no raw OS error is
/// one Flux raised itself (an interior NUL), not the filesystem. Sound only for a
/// failure at `CopyStep::Publish`: the temporary's name contains the target's, so a
/// name the filesystem rejects fails at `Create` first.
fn primitive_unavailable(e: &std::io::Error) -> bool {
    e.kind() == ErrorKind::Unsupported
        || (cfg!(unix) && e.kind() == ErrorKind::InvalidInput && e.raw_os_error().is_some())
}

/// The §129 pre-flight: the source root against the RESOLVED destination anchor
/// (decision 2). A destination symlinked to the source is caught here, by identity.
fn preflight(
    src: FileIdentity,
    anchor: FileIdentity,
    safety: Safety,
    warnings: &mut WeakIdentityWarnings,
) -> std::result::Result<(), CopyError> {
    match (src, anchor) {
        (FileIdentity::Strong(a), FileIdentity::Strong(b)) if a == b => {
            Err(refuse("the destination resolves to the source, by identity"))
        }
        (FileIdentity::Strong(_), FileIdentity::Strong(_)) => Ok(()),
        (s, d) => match safety {
            Safety::Default => {
                warnings.record(weaker(s, d), Path::new(""));
                Ok(())
            }
            Safety::Strict => Err(refuse(
                "the source cannot be compared with the destination with full confidence",
            )),
        },
    }
}

/// `inner` equals `outer` or lies inside it, compared component by component with `.`
/// dropped and no filesystem access. `..` is not resolved (that needs the
/// filesystem): a spelling through `..` is compared as written, and identity is the
/// check that sees through it. Paths are compared as given; cut 5's CLI canonicalizes
/// both roots before calling the engine.
fn lexically_within(inner: &Path, outer: &Path) -> bool {
    fn parts(p: &Path) -> Vec<Component<'_>> {
        p.components().filter(|c| !matches!(c, Component::CurDir)).collect()
    }
    let (i, o) = (parts(inner), parts(outer));
    i.len() >= o.len() && i[..o.len()] == o[..]
}

fn refuse(why: &'static str) -> CopyError {
    CopyError::at(CopyStep::Resolve, FsError::new(Code::SafetyRejected, std::io::Error::other(why)))
}

/// Count the failure, then stream it. The only place either happens.
fn report(
    out: &mut TreeOutcome,
    on_failure: &mut dyn FnMut(TreeFailure),
    path: PathBuf,
    cause: TreeFailureCause,
) {
    out.failures.count(&cause);
    on_failure(TreeFailure { path, cause });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fault_fs::FaultFs;
    use flux_fs::{Code, Durability, FileSystem, ObjectId, OperationId, Preserve};

    #[test]
    fn the_tally_counts_each_cause_in_its_own_field() {
        let e = || FsError::new(Code::IoError, std::io::Error::other("x"));
        let mut t = FailureTally::default();
        t.count(&TreeFailureCause::Walk(e()));
        t.count(&TreeFailureCause::CreateDir(e()));
        t.count(&TreeFailureCause::Copy(CopyError::at(crate::copy::CopyStep::Create, e())));
        t.count(&TreeFailureCause::Unsupported(FileType::Symlink));
        t.count(&TreeFailureCause::PublishedWithComplaints(Vec::new()));
        assert_eq!(
            t,
            FailureTally {
                walk: 1,
                create_dir: 1,
                copy: 1,
                unsupported: 1,
                published_with_complaints: 1
            }
        );
        assert_eq!(t.total(), 5);
        assert!(!t.is_empty());
        assert!(FailureTally::default().is_empty());
    }

    #[test]
    fn warnings_group_weak_by_volume_and_keep_the_first_example() {
        let weak = |volume| FileIdentity::Weak(ObjectId { volume, index: 1 });
        let mut w = WeakIdentityWarnings::default();
        w.record(weak(7), Path::new("a"));
        w.record(weak(7), Path::new("b"));
        w.record(weak(9), Path::new("c"));
        w.record(FileIdentity::Unavailable, Path::new("d"));
        w.record(FileIdentity::Strong(ObjectId { volume: 1, index: 1 }), Path::new("e"));

        assert_eq!(w.weak[&7], DegradedGroup { count: 2, example: PathBuf::from("a") });
        assert_eq!(w.weak[&9], DegradedGroup { count: 1, example: PathBuf::from("c") });
        assert_eq!(w.unavailable, Some(DegradedGroup { count: 1, example: PathBuf::from("d") }));
        assert!(!w.is_empty());
        assert!(WeakIdentityWarnings::default().is_empty());
    }

    fn opts() -> CopyOptions {
        CopyOptions {
            preserve_times: Preserve::Default,
            preserve_permissions: Preserve::Default,
            durability: Durability::Normal,
            publish: Publish::Replace,
            safety: Safety::Default,
            operation_id: OperationId::new("op1"),
        }
    }

    fn strict() -> CopyOptions {
        let mut o = opts();
        o.safety = Safety::Strict;
        o
    }

    fn run(
        fs: &FaultFs,
        src: &str,
        dst: &str,
        o: &CopyOptions,
    ) -> (std::result::Result<TreeOutcome, CopyError>, Vec<TreeFailure>) {
        let mut got = Vec::new();
        let r = copy_tree(fs, Path::new(src), Path::new(dst), o, &mut |f| got.push(f));
        (r, got)
    }

    /// `/src/a` (1 byte) and `/src/sub/b` (2 bytes). `/dst` absent.
    fn tree() -> FaultFs {
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/src")).unwrap();
        fs.create_dir(Path::new("/src/sub")).unwrap();
        fs.write_file("/src/a", b"A");
        fs.write_file("/src/sub/b", b"BB");
        fs
    }

    fn identity_of(fs: &FaultFs, p: &str) -> FileIdentity {
        fs.metadata(Path::new(p)).unwrap().identity
    }

    fn calls_since(fs: &FaultFs, n: usize) -> Vec<String> {
        fs.calls()[n..].to_vec()
    }

    #[test]
    fn lexically_within_compares_components() {
        assert!(lexically_within(Path::new("/data"), Path::new("/data")));
        assert!(lexically_within(Path::new("/data/backup"), Path::new("/data")));
        assert!(lexically_within(Path::new("/data/./backup"), Path::new("/data")));
        assert!(!lexically_within(Path::new("/database"), Path::new("/data")));
        assert!(!lexically_within(Path::new("/other"), Path::new("/data")));
    }

    #[test]
    fn a_missing_source_root_fails_the_whole_operation_before_the_destination() {
        let fs = FaultFs::new();
        let (r, got) = run(&fs, "/src", "/dst", &opts());
        let e = r.unwrap_err();
        assert_eq!(e.step, CopyStep::Source);
        assert!(got.is_empty());
        assert!(!fs.called("create_dir"));
    }

    #[test]
    fn a_destination_inside_the_source_is_refused_lexically_before_any_call() {
        for dst in ["/src", "/src/backup"] {
            let fs = tree();
            // A WEAK source identity, so the identity pre-flight cannot catch the overlap
            // (it degrades under Default and proceeds): the lexical floor is then the only
            // guard, and a mutant that disables it creates `/src/backup` and goes red.
            // Without this, the pre-flight refuses both spellings on its own (the anchor IS
            // `/src`) and the test could not tell the floor from the pre-flight (plan
            // panel round 1).
            fs.set_identity("/src", FileIdentity::Weak(ObjectId { volume: 5, index: 1 }));
            let n = fs.calls().len();
            let (r, _) = run(&fs, "/src", dst, &opts());
            assert_eq!(r.unwrap_err().code(), Code::SafetyRejected, "{dst}");
            assert!(
                !calls_since(&fs, n).iter().any(|c| c.starts_with("create_dir")),
                "{dst}: nothing created"
            );
        }
    }

    #[test]
    fn a_destination_that_is_the_source_by_identity_is_refused_before_anything_is_created() {
        let fs = tree();
        fs.create_dir(Path::new("/dst")).unwrap();
        fs.set_identity("/dst", identity_of(&fs, "/src"));
        let n = fs.calls().len();

        let (r, got) = run(&fs, "/src", "/dst", &opts());

        assert_eq!(r.unwrap_err().code(), Code::SafetyRejected);
        assert!(got.is_empty());
        assert!(!calls_since(&fs, n).iter().any(|c| c.starts_with("create")));
    }

    #[test]
    fn an_absent_destination_is_anchored_on_its_parent() {
        let fs = tree();
        fs.create_dir(Path::new("/out")).unwrap();
        fs.set_identity("/out", identity_of(&fs, "/src"));

        let (r, _) = run(&fs, "/src", "/out/copy", &opts());

        assert_eq!(r.unwrap_err().code(), Code::SafetyRejected);
        assert!(!fs.exists("/out/copy"));
    }

    #[test]
    fn a_file_at_the_destination_root_is_refused_as_not_a_directory() {
        let fs = tree();
        fs.write_file("/dst", b"a file");
        let (r, _) = run(&fs, "/src", "/dst", &opts());
        let e = r.unwrap_err();
        assert_eq!(e.cause.source.kind(), std::io::ErrorKind::NotADirectory);
        assert_eq!(fs.read_file("/dst").as_deref(), Some(&b"a file"[..]));
    }

    #[test]
    fn a_weak_preflight_warns_under_default_and_refuses_under_strict() {
        let weak = FileIdentity::Weak(ObjectId { volume: 9, index: 1 });

        let lax = tree();
        lax.set_identity("/src", weak);
        let (r, _) = run(&lax, "/src", "/dst", &opts());
        let out = r.unwrap();
        assert_eq!(out.warnings.weak[&9].example, PathBuf::new(), "the root is the empty path");

        let tight = tree();
        tight.set_identity("/src", weak);
        let (r, _) = run(&tight, "/src", "/dst", &strict());
        assert_eq!(r.unwrap_err().code(), Code::SafetyRejected);
        assert!(!tight.exists("/dst"));
    }

    #[test]
    fn a_fresh_tree_is_copied_through_handles_with_no_replace() {
        let fs = tree();
        let (r, got) = run(&fs, "/src", "/dst", &opts());
        let out = r.unwrap();

        assert!(got.is_empty(), "{got:?}");
        assert_eq!((out.files_copied, out.bytes_copied, out.directories_created), (2, 3, 2));
        assert!(out.failures.is_empty());
        assert!(out.warnings.is_empty());
        assert_eq!(fs.read_file("/dst/a").as_deref(), Some(&b"A"[..]));
        assert_eq!(fs.read_file("/dst/sub/b").as_deref(), Some(&b"BB"[..]));
        assert!(fs.called("rename_no_replace"), "NoReplace is mandatory in a tree");
        assert!(!fs.called("rename_replace"), "the caller's Replace is overridden");
    }

    #[test]
    fn an_existing_destination_directory_is_merged_and_not_counted() {
        let fs = tree();
        fs.create_dir(Path::new("/dst")).unwrap();
        fs.create_dir(Path::new("/dst/sub")).unwrap();
        let (r, got) = run(&fs, "/src", "/dst", &opts());
        let out = r.unwrap();
        assert!(got.is_empty(), "{got:?}");
        assert_eq!((out.files_copied, out.directories_created), (2, 0));
        assert_eq!(fs.read_file("/dst/sub/b").as_deref(), Some(&b"BB"[..]));
    }

    #[test]
    fn an_existing_destination_file_is_left_intact_and_reported_as_a_collision() {
        let fs = tree();
        fs.create_dir(Path::new("/dst")).unwrap();
        fs.write_file("/dst/a", b"old");

        let (r, got) = run(&fs, "/src", "/dst", &opts());
        let out = r.unwrap();

        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].path, Path::new("a"));
        match &got[0].cause {
            TreeFailureCause::Copy(e) => {
                assert_eq!(e.code(), Code::DestinationNamespaceCollision);
                assert_eq!(e.step, CopyStep::Gate);
            }
            other => panic!("expected Copy, got {other:?}"),
        }
        assert_eq!(fs.read_file("/dst/a").as_deref(), Some(&b"old"[..]));
        assert!(
            !fs.calls().iter().any(|c| c.starts_with("create_new") && c.contains("a.flux-partial")),
            "the bytes were never copied"
        );
        assert_eq!(out.failures.copy, 1);
        assert_eq!(fs.read_file("/dst/sub/b").as_deref(), Some(&b"BB"[..]), "the walk continued");
    }

    #[test]
    fn a_file_that_is_the_source_by_identity_fails_alone_and_the_walk_continues() {
        let fs = tree();
        fs.create_dir(Path::new("/dst")).unwrap();
        fs.write_file("/dst/a", b"A");
        fs.set_identity("/dst/a", identity_of(&fs, "/src/a"));

        let (r, got) = run(&fs, "/src", "/dst", &opts());

        let out = r.unwrap();
        assert_eq!(got.len(), 1);
        match &got[0].cause {
            TreeFailureCause::Copy(e) => assert_eq!(e.code(), Code::SafetyRejected),
            other => panic!("expected Copy, got {other:?}"),
        }
        assert_eq!(out.files_copied, 1);
        assert_eq!(fs.read_file("/dst/sub/b").as_deref(), Some(&b"BB"[..]));
    }

    #[test]
    fn two_source_directories_folded_into_one_destination_are_a_collision() {
        // `A` and `a` in one source directory; the destination "folds" them: `/dst/a`
        // pre-exists carrying the identity `/dst/A` will get when this operation makes it.
        let fs = FaultFs::new();
        for d in ["/src", "/src/A", "/src/a", "/dst", "/dst/a"] {
            fs.create_dir(Path::new(d)).unwrap();
        }
        fs.write_file("/src/A/x", b"x");
        fs.write_file("/src/a/y", b"y");
        let folded = FileIdentity::Strong(ObjectId { volume: 1, index: 9_003 });
        fs.set_identity("/dst/A", folded);
        fs.set_identity("/dst/a", folded);

        let (r, got) = run(&fs, "/src", "/dst", &opts());
        let out = r.unwrap();

        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].path, Path::new("a"));
        match &got[0].cause {
            TreeFailureCause::CreateDir(e) => {
                assert_eq!(e.code, Code::DestinationNamespaceCollision)
            }
            other => panic!("expected CreateDir, got {other:?}"),
        }
        assert!(fs.exists("/dst/A/x"));
        assert!(!fs.exists("/dst/a/y"), "the folded subtree was not merged");
        assert_eq!(out.failures.create_dir, 1);
    }

    #[test]
    fn a_source_directory_that_is_the_destination_aborts_the_operation() {
        let fs = tree();
        fs.create_dir(Path::new("/dst")).unwrap();
        fs.set_identity("/src/sub", identity_of(&fs, "/dst"));

        let (r, _) = run(&fs, "/src", "/dst", &opts());

        assert_eq!(r.unwrap_err().code(), Code::SafetyRejected);
        assert!(!fs.exists("/dst/sub/b"));
    }

    #[test]
    fn a_weak_directory_warns_under_default_and_aborts_under_strict() {
        let weak = FileIdentity::Weak(ObjectId { volume: 7, index: 1 });

        let lax = tree();
        lax.set_identity("/src/sub", weak);
        let (r, got) = run(&lax, "/src", "/dst", &opts());
        let out = r.unwrap();
        assert!(got.is_empty());
        assert_eq!(
            out.warnings.weak[&7],
            DegradedGroup { count: 1, example: PathBuf::from("sub") }
        );

        let tight = tree();
        tight.set_identity("/src/sub", weak);
        let (r, _) = run(&tight, "/src", "/dst", &strict());
        assert_eq!(r.unwrap_err().code(), Code::SafetyRejected);
    }

    #[test]
    fn a_file_whose_destination_cannot_be_inspected_lands_in_the_unavailable_bucket() {
        // metadata calls in order: walk's root stat (1), copy_tree's source identity (2),
        // copy_file_at's source stat (3), the Step 2a gate's destination stat (4).
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/src")).unwrap();
        fs.create_dir(Path::new("/dst")).unwrap();
        fs.write_file("/src/a", b"A");
        fs.fail_nth("metadata", 4, Code::PermissionDenied, std::io::ErrorKind::PermissionDenied);

        let (r, got) = run(&fs, "/src", "/dst", &opts());
        let out = r.unwrap();

        let stats: Vec<_> = fs.calls().into_iter().filter(|c| c.starts_with("metadata(")).collect();
        assert!(stats[3].contains("dst"), "the 4th stat is the gate's: {stats:?}");
        assert!(got.is_empty(), "{got:?}");
        assert_eq!(out.files_copied, 1);
        assert_eq!(
            out.warnings.unavailable,
            Some(DegradedGroup { count: 1, example: PathBuf::from("a") })
        );
    }

    #[test]
    fn a_destination_without_the_no_replace_primitive_aborts_at_the_first_publish() {
        let fs = tree();
        fs.set_no_replace_support(false);

        let (r, got) = run(&fs, "/src", "/dst", &opts());

        let e = r.unwrap_err();
        assert_eq!(e.code(), Code::NoReplacePublishUnavailable);
        assert_eq!(e.step, CopyStep::Publish);
        assert!(got.is_empty());
        assert!(!fs.exists("/dst/a.flux-partial.op1"), "the temporary was discarded");
        assert!(!fs.exists("/dst/sub"), "stopped at the first file, before `sub`");
    }

    #[test]
    fn a_create_failure_is_one_failure_not_an_abort_even_if_it_says_unsupported() {
        // Decision 3 is sound only at the PUBLISH step: `Unsupported` at `create_new`
        // is one file's failure. Without the step guard this would abort the tree.
        let fs = tree();
        fs.fail_kind("create_new", Code::IoError, std::io::ErrorKind::Unsupported);

        let (r, got) = run(&fs, "/src", "/dst", &opts());

        let out = r.unwrap();
        assert_eq!(got.len(), 1);
        match &got[0].cause {
            TreeFailureCause::Copy(e) => assert_eq!(e.step, CopyStep::Create),
            other => panic!("expected Copy, got {other:?}"),
        }
        assert_eq!(out.files_copied, 1);
    }

    #[test]
    fn primitive_unavailable_is_unsupported_or_a_raw_einval_only() {
        assert!(primitive_unavailable(&std::io::Error::from(std::io::ErrorKind::Unsupported)));
        assert!(!primitive_unavailable(&std::io::Error::from(std::io::ErrorKind::InvalidInput)));
        assert!(!primitive_unavailable(&std::io::Error::from(std::io::ErrorKind::AlreadyExists)));
        #[cfg(unix)]
        assert!(primitive_unavailable(&std::io::Error::from_raw_os_error(22)), "EINVAL");
    }

    #[test]
    fn a_metadata_complaint_is_reported_with_the_file_published() {
        let fs = tree();
        fs.fail("set_times", Code::PermissionDenied);

        let (r, got) = run(&fs, "/src", "/dst", &opts());

        let out = r.unwrap();
        assert_eq!(got.len(), 1);
        match &got[0].cause {
            TreeFailureCause::PublishedWithComplaints(v) => assert_eq!(v.len(), 1),
            other => panic!("expected PublishedWithComplaints, got {other:?}"),
        }
        assert!(fs.exists(Path::new("/dst").join(&got[0].path)), "the file is there");
        assert_eq!(out.files_copied, 2);
        assert_eq!(out.failures.published_with_complaints, 1);
    }

    #[test]
    fn a_symlink_is_reported_unsupported_and_never_copied() {
        let fs = tree();
        fs.add_symlink("/src/link");

        let (r, got) = run(&fs, "/src", "/dst", &opts());

        let out = r.unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].path, Path::new("link"));
        assert!(matches!(got[0].cause, TreeFailureCause::Unsupported(FileType::Symlink)));
        assert!(!fs.exists("/dst/link"));
        assert_eq!(out.failures.unsupported, 1);
    }

    #[test]
    fn a_walk_error_is_reported_and_the_walk_continues() {
        let fs = tree();
        // read_dir calls: the walk's root listing (1), then `sub` (2).
        fs.fail_nth("read_dir", 2, Code::PermissionDenied, std::io::ErrorKind::PermissionDenied);

        let (r, got) = run(&fs, "/src", "/dst", &opts());

        let out = r.unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].path, Path::new("sub"));
        assert!(matches!(got[0].cause, TreeFailureCause::Walk(_)));
        assert_eq!(fs.read_file("/dst/a").as_deref(), Some(&b"A"[..]));
        assert!(!fs.exists("/dst/sub"), "the walk emitted no Dir for it");
        assert_eq!(out.failures.walk, 1);
    }

    #[test]
    fn a_failed_directory_is_reported_once_and_its_descendants_never_copied() {
        let fs = tree();
        fs.create_dir(Path::new("/src/sub/deeper")).unwrap();
        fs.write_file("/src/sub/deeper/c", b"c");
        fs.create_dir(Path::new("/dst")).unwrap();
        fs.write_file("/dst/sub", b"a file where a directory belongs");

        let (r, got) = run(&fs, "/src", "/dst", &opts());

        let out = r.unwrap();
        assert_eq!(got.len(), 1, "reported once: {got:?}");
        assert_eq!(got[0].path, Path::new("sub"));
        assert!(matches!(got[0].cause, TreeFailureCause::CreateDir(_)));
        assert!(
            !fs.calls().iter().any(|c| c.contains("flux-partial") && !c.contains("a.flux-partial")),
            "nothing under `sub` reached copy_file_at: {:?}",
            fs.calls()
        );
        assert_eq!(out.files_copied, 1);
    }

    #[test]
    fn a_leftover_temporary_is_reported_by_its_destination_relative_path() {
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/src")).unwrap();
        fs.create_dir(Path::new("/src/sub")).unwrap();
        fs.write_file("/src/sub/b", b"BB");
        fs.fail("rename_no_replace", Code::PermissionDenied);
        fs.fail_always("remove_file", Code::PermissionDenied);

        let (r, got) = run(&fs, "/src", "/dst", &opts());

        r.unwrap();
        match &got[0].cause {
            TreeFailureCause::Copy(e) => {
                let (path, _) = e.leftover.as_ref().expect("the leftover is reported");
                assert_eq!(path, Path::new("sub/b.flux-partial.op1"));
            }
            other => panic!("expected Copy, got {other:?}"),
        }
    }
}
