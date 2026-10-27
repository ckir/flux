// Copyright (c) 2026 Costas Kirgoussios
//
// This software is licensed under the PolyForm Noncommercial License 1.0.0.
// You may obtain a copy of the License at:
// https://polyformproject.org/licenses/noncommercial/1.0.0/

//! `flux verify`: compare two trees without writing anything (design 6.2).
//!
//! Two walks, one per root, are merged in lockstep by `FluxPathKey` bytes. Each record is handed to the sink as soon
//! as it is decided; the engine holds no list of records.

use crate::run::{RunError, SourceKind, SourceSpec, plan_roots};
use crate::walk::{DEFAULT_MAX_DEPTH, MountPolicy, Walk, WalkEvent, WalkOptions, walk_with};
use flux_fs::{
    Code, DestinationRoot, DirHandle, FileIdentity, FileSystem, FileType, FluxPathKey, FsError,
    HashAlgorithm, Links, ObjectId, Safety,
};
use std::cmp::Ordering;
use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

/// What a path turned out to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Match,
    Missing,
    Mismatched,
    Extra,
    Unreadable,
}

impl Outcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Outcome::Match => "match",
            Outcome::Missing => "missing",
            Outcome::Mismatched => "mismatched",
            Outcome::Extra => "extra",
            Outcome::Unreadable => "unreadable",
        }
    }
}

/// The kind of entry a record is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    File,
    Dir,
    Symlink,
    Other,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::File => "file",
            Kind::Dir => "dir",
            Kind::Symlink => "symlink",
            Kind::Other => "other",
        }
    }
}

/// One decided path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    /// Relative to the walk root; `.` for the root record (design 6.6). For a file verify, the destination's file name (R4).
    pub path: PathBuf,
    pub outcome: Outcome,
    /// The SOURCE side's kind when it exists, else the DEST side's (V9).
    pub kind: Kind,
    /// `Some(Code::VerifyMismatch)` for every `mismatched`; the error's code for `unreadable` and for a DEST-only
    /// `extra` directory that could not be listed; `None` otherwise.
    pub code: Option<Code>,
    /// Error text for `unreadable` and for that `extra`; `"source {a}, destination {b}"` for a type mismatch; `"{s} != {d}"` (first 8 hex
    /// characters of each digest) for a file mismatch; `"symlink payloads differ"`; `MOUNT_NOT_ENTERED`; else empty.
    pub detail: String,
}

/// The detail of a `match` record for a directory that is a mount point neither walker entered (design 6.2).
pub const MOUNT_NOT_ENTERED: &str = "mount point not entered";
/// The detail of an `unreadable` record for two `Other` entries (design 6.3).
pub const SPECIAL_NOT_COMPARED: &str = "special file not compared";

/// The counts of a run.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Summary {
    pub matched: u64,
    pub missing: u64,
    pub mismatched: u64,
    pub extra: u64,
    pub unreadable: u64,
    /// Mount points not entered (each is also one `matched`).
    pub not_entered: u64,
    /// Source entries the `--exclude` patterns removed (the source side only; `--no-recursive` is silent).
    pub excluded: u64,
    /// Bytes read by completed hashes on both sides; a read that failed adds nothing.
    pub bytes_hashed: u64,
}

impl Summary {
    /// Whether the run exits 1: `extra` never fails it.
    pub fn failed(&self) -> bool {
        self.missing + self.mismatched + self.unreadable > 0
    }
}

/// How a verify is configured.
#[derive(Debug, Clone)]
pub struct VerifyOptions {
    pub links: Links,
    pub mounts: MountPolicy,
    pub hash: HashAlgorithm,
    /// Applied to BOTH walks (design 5.6).
    pub selection: flux_fs::Selection,
}

/// Why a verify did not run.
#[derive(Debug)]
pub enum VerifyError {
    /// Exit 2: the two roots are the same object, or one lies inside the other; nothing was read (design 6.1).
    RefusedRoots { reason: String },
    /// Exit 2 (several sources): the root check of the source named `name` refused; `reason` is the clause without its
    /// `; nothing was compared` tail, the by-path-alone note (if any) kept. Nothing was walked.
    RefusedRoot { name: OsString, reason: String },
    /// Exit 2 (several sources): the planner refused the names; `reason` is its detail.
    RefusedNames { reason: String },
    /// Exit 2 (several sources): two roots are one directory on this filesystem.
    RefusedAlias { a: OsString, b: OsString },
}

/// One entry of one side's walk, in the form the merge compares.
struct Item {
    path: PathBuf,
    key: Vec<u8>,
    kind: Kind,
    not_entered: bool,
    failed: Option<FsError>,
}

type RawEvent = std::result::Result<WalkEvent, crate::walk::WalkError>;

/// Which of Flux's own control state a side drops from the merge (design 6.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlFilter {
    /// Nothing is dropped (the source side).
    None,
    /// A destination that is itself a Flux root: the top-level `.flux` directory, the root-level `.flux-root.lock`, and
    /// a copy's partial temporaries.
    Root,
    /// A destination root nested inside another: only a copy's partial temporaries; a `.flux` there is user data.
    NestedRoot,
}

/// One root's walk, reduced to `Item`s, with one held item for the merge to look at.
struct Side<'a, F: FileSystem> {
    walk: Option<Walk<'a, F>>,
    /// One event of lookahead (a `Dir` is followed by its `MountSkipped`).
    peeked: Option<Option<RawEvent>>,
    held: Option<Item>,
    done: bool,
    pruned: Option<PathBuf>,
    /// The SOURCE side under `Links::Skip`: symlink items are dropped (design 6.3).
    skip_links: bool,
    /// The DEST side: which of Flux's own control state is dropped (design 6.4).
    filter: ControlFilter,
}

impl<'a, F: FileSystem> Side<'a, F> {
    fn new(
        fs: &'a F,
        root: &Path,
        mounts: MountPolicy,
        skip_links: bool,
        filter: ControlFilter,
        selection: &'a flux_fs::Selection,
    ) -> Result<Self, FsError> {
        let mut walk = walk_with(fs, root, WalkOptions { max_depth: DEFAULT_MAX_DEPTH, mounts })?;
        walk.set_selection(selection);
        Ok(Side {
            walk: Some(walk),
            peeked: None,
            held: None,
            done: false,
            pruned: None,
            skip_links,
            filter,
        })
    }

    /// The entries this side's walk dropped under `--exclude` (0 for a side with no walk).
    fn excluded(&self) -> u64 {
        self.walk.as_ref().map_or(0, Walk::excluded)
    }

    /// A side with nothing on it: the walk of a destination that does not exist (design 6.1).
    fn empty() -> Self {
        Side {
            walk: None,
            peeked: None,
            held: None,
            done: true,
            pruned: None,
            skip_links: false,
            filter: ControlFilter::None,
        }
    }

    fn raw(&mut self) -> Option<RawEvent> {
        if let Some(p) = self.peeked.take() {
            return p;
        }
        self.walk.as_mut().and_then(Iterator::next)
    }

    fn peek_raw(&mut self) -> &Option<RawEvent> {
        if self.peeked.is_none() {
            let next = self.walk.as_mut().and_then(Iterator::next);
            self.peeked = Some(next);
        }
        self.peeked.as_ref().expect("filled above")
    }

    fn item(path: PathBuf, kind: Kind, not_entered: bool, failed: Option<FsError>) -> Item {
        match FluxPathKey::from_relative(&path) {
            Ok(k) => Item { path, key: k.0, kind, not_entered, failed },
            Err(e) => Item { path, key: Vec::new(), kind, not_entered, failed: Some(e) },
        }
    }

    /// Whether `item` is Flux's own control state under `filter` (design 6.4). `None` drops nothing. `Root` drops a
    /// top-level `.flux` directory with all it holds, the root-level `.flux-root.lock`, and a copy's
    /// `<name>.flux-partial.<id>` temporary at any depth. `NestedRoot` drops only that temporary.
    fn is_control_state(item: &Item, filter: ControlFilter) -> bool {
        match filter {
            ControlFilter::None => false,
            ControlFilter::Root => Self::is_root_control_state(item),
            ControlFilter::NestedRoot => Self::is_partial_temporary(item),
        }
    }

    fn is_root_control_state(item: &Item) -> bool {
        let mut parts = item.path.components();
        let first = parts.next().map(|c| c.as_os_str());
        let single = parts.next().is_none();
        if first.is_some_and(|f| f.as_encoded_bytes() == crate::state::FLUX_DIR.as_bytes())
            && (!single || item.kind == Kind::Dir)
        {
            return true;
        }
        if single
            && item.kind == Kind::File
            && first.is_some_and(|f| {
                f.as_encoded_bytes() == crate::lock::site::ROOT_LOCK_NAME.as_bytes()
            })
        {
            return true;
        }
        Self::is_partial_temporary(item)
    }

    fn is_partial_temporary(item: &Item) -> bool {
        matches!(item.kind, Kind::File | Kind::Symlink)
            && item
                .path
                .file_name()
                .is_some_and(|n| crate::state::id_after(n, crate::state::PARTIAL_INFIX).is_some())
    }

    fn under_pruned(&self, path: &Path) -> bool {
        self.pruned.as_ref().is_some_and(|p| path.starts_with(p))
    }

    /// The next item the merge has not yet seen, skipping what a prune covers.
    fn advance(&mut self) -> Option<Item> {
        loop {
            let Some(ev) = self.raw() else {
                self.done = true;
                return None;
            };
            let item = match ev {
                // Every `WalkError` is a directory today: the walker raises one only from `enter` (listing a
                // directory), `walk.rs`.
                Err(we) => Self::item(we.path, Kind::Dir, false, Some(we.cause)),
                Ok(WalkEvent::Dir { path, .. }) => {
                    let skipped = matches!(
                        self.peek_raw(),
                        Some(Ok(WalkEvent::MountSkipped { path: p })) if *p == path
                    );
                    if skipped {
                        self.peeked = None;
                    }
                    Self::item(path, Kind::Dir, skipped, None)
                }
                Ok(WalkEvent::File { path }) => Self::item(path, Kind::File, false, None),
                Ok(WalkEvent::Symlink { path }) => {
                    if self.skip_links {
                        continue;
                    }
                    Self::item(path, Kind::Symlink, false, None)
                }
                Ok(WalkEvent::Other { path }) => Self::item(path, Kind::Other, false, None),
                Ok(WalkEvent::DirEnd { .. })
                | Ok(WalkEvent::MountSkipped { .. })
                | Ok(WalkEvent::MountUnknown { .. }) => continue,
            };
            if self.under_pruned(&item.path) || Self::is_control_state(&item, self.filter) {
                continue;
            }
            return Some(item);
        }
    }

    fn peek(&mut self) -> Option<&Item> {
        if self.held.is_none() && !self.done {
            self.held = self.advance();
        }
        self.held.as_ref()
    }

    fn take(&mut self) -> Option<Item> {
        self.peek();
        self.held.take()
    }

    /// Skip every later item under `path`, and the held one if it lies there.
    fn prune(&mut self, path: &Path) {
        self.pruned = Some(path.to_path_buf());
        if self.held.as_ref().is_some_and(|h| h.path.starts_with(path)) {
            self.held = None;
        }
    }
}

/// The single place a record is counted and sent.
struct Emitter<'s> {
    summary: Summary,
    sink: &'s mut dyn FnMut(Record),
}

impl Emitter<'_> {
    fn emit(&mut self, record: Record) {
        let s = &mut self.summary;
        match record.outcome {
            Outcome::Match => s.matched += 1,
            Outcome::Missing => s.missing += 1,
            Outcome::Mismatched => s.mismatched += 1,
            Outcome::Extra => s.extra += 1,
            Outcome::Unreadable => s.unreadable += 1,
        }
        (self.sink)(record);
    }

    fn alone(&mut self, item: &Item, outcome: Outcome) {
        self.emit(Record {
            path: item.path.clone(),
            outcome,
            kind: item.kind,
            code: None,
            detail: String::new(),
        });
    }

    fn unreadable(&mut self, path: PathBuf, kind: Kind, code: Code, detail: String) {
        self.emit(Record { path, outcome: Outcome::Unreadable, kind, code: Some(code), detail });
    }
}

/// Read `path` in full; the digest and the byte count, or the code and text of the failure.
fn hash_file<F: FileSystem>(
    fs: &F,
    path: &Path,
    buf: &mut [u8],
) -> Result<(flux_hash::Digest, u64), (Code, String)> {
    let mut reader = fs.open_read(path).map_err(|e| (e.code, e.source.to_string()))?;
    flux_hash::hash_reader(&mut reader, buf, &mut || Ok(()))
        .map_err(|e| (FsError::classify(&e), e.to_string()))
}

/// Compare two paired files: source first, and DEST is not opened when the source failed. `Ok(None)` when the digests
/// are equal, `Ok(Some(detail))` when they differ, `Err` with the code and text of a read that failed.
fn compare_files<F: FileSystem>(
    fs: &F,
    src: &Path,
    dst: &Path,
    buf: &mut [u8],
    bytes_hashed: &mut u64,
) -> Result<Option<String>, (Code, String)> {
    let (s, sn) = hash_file(fs, src, buf)?;
    *bytes_hashed += sn;
    let (d, dn) = hash_file(fs, dst, buf)?;
    *bytes_hashed += dn;
    Ok((s != d).then(|| format!("{} != {}", &s.to_hex()[..8], &d.to_hex()[..8])))
}

/// Merge the walks of `src` and `dst` into records, streamed to `sink` in path order.
pub fn verify_tree<F: DestinationRoot>(
    fs: &F,
    src: &Path,
    dst: &Path,
    opts: &VerifyOptions,
    sink: &mut dyn FnMut(Record),
) -> Result<Summary, VerifyError> {
    verify_tree_notifying(fs, src, dst, opts, &mut || {}, sink)
}

/// `verify_tree`, calling `on_absent` once, after the root check passed and before the first read of either tree, when
/// the destination does not exist (and is verified as empty). A refused run never calls it.
pub fn verify_tree_notifying<F: DestinationRoot>(
    fs: &F,
    src: &Path,
    dst: &Path,
    opts: &VerifyOptions,
    on_absent: &mut dyn FnMut(),
    sink: &mut dyn FnMut(Record),
) -> Result<Summary, VerifyError> {
    verify_tree_filtered(fs, src, dst, opts, ControlFilter::Root, on_absent, sink)
}

/// `verify_tree_notifying` with the destination side's control-state `filter` chosen by the caller.
pub(crate) fn verify_tree_filtered<F: DestinationRoot>(
    fs: &F,
    src: &Path,
    dst: &Path,
    opts: &VerifyOptions,
    filter: ControlFilter,
    on_absent: &mut dyn FnMut(),
    sink: &mut dyn FnMut(Record),
) -> Result<Summary, VerifyError> {
    let mut out = Emitter { summary: Summary::default(), sink: &mut *sink };
    let verdict =
        check_pair(fs, src, dst).map_err(|reason| VerifyError::RefusedRoots { reason })?;
    if matches!(verdict, RootVerdict::Absent) {
        on_absent();
    }
    walk_checked(fs, src, dst, opts, filter, verdict, &mut out);
    Ok(out.summary)
}

const NOTHING_COMPARED: &str = "; nothing was compared";

/// A root-check refusal `reason` without its `; nothing was compared` tail; what follows the tail is kept.
fn strip_compared_tail(reason: &str) -> String {
    match reason.split_once(NOTHING_COMPARED) {
        Some((head, rest)) => format!("{head}{rest}"),
        None => reason.to_string(),
    }
}

/// Several sources against `dest` (design 6.2 to 6.5): each root is compared with `dest/<name>`, in the order of the
/// copy's root planner. Every root is checked before any is walked, so a refusal prints nothing. A folder root's paths
/// are re-rooted under its name; a file root's single record already carries it. `on_absent` fires for a folder
/// root whose destination does not exist, `on_root_done` after each root with that root's counts.
pub fn verify_several<F: DestinationRoot>(
    fs: &F,
    specs: Vec<SourceSpec>,
    dest: &Path,
    opts: &VerifyOptions,
    on_absent: &mut dyn FnMut(&OsStr),
    on_root_done: &mut dyn FnMut(&OsStr, &Summary),
    sink: &mut dyn FnMut(Record),
) -> Result<Summary, VerifyError> {
    let specs = plan_roots(specs).map_err(|e| match e {
        RunError::Refused { refusal, .. } => VerifyError::RefusedNames { reason: refusal.detail },
        RunError::Failed { .. } => unreachable!("the root planner only refuses"),
    })?;
    let mut verdicts = Vec::with_capacity(specs.len());
    for spec in &specs {
        let verdict = match spec.kind {
            SourceKind::Folder => {
                let dst = dest.join(&spec.name);
                // The destination's own type, not its target's: a link at `DEST/<name>` is a wrong type.
                match fs.metadata(&dst) {
                    Ok(m) if m.file_type != FileType::Dir => {
                        Some(RootVerdict::WrongType(file_kind(m.file_type)))
                    }
                    Err(e) if e.source.kind() != std::io::ErrorKind::NotFound => {
                        Some(RootVerdict::Unreadable { side: DESTINATION, error: e })
                    }
                    _ => Some(check_pair(fs, &spec.src, &dst).map_err(|reason| {
                        VerifyError::RefusedRoot {
                            name: spec.name.clone(),
                            reason: strip_compared_tail(&reason),
                        }
                    })?),
                }
            }
            SourceKind::File => None,
        };
        verdicts.push(verdict);
    }
    // Two folder roots that are one directory on the destination (design 6.2): the first name per Strong identity.
    // Run after the whole loop so a refused root wins, and before any walk so nothing remembered is emitted.
    let mut seen: HashMap<ObjectId, OsString> = HashMap::new();
    for (spec, verdict) in specs.iter().zip(&verdicts) {
        if let Some(RootVerdict::Checked { dest_identity: Some(id) }) = verdict {
            match seen.get(id) {
                Some(a) => {
                    return Err(VerifyError::RefusedAlias { a: a.clone(), b: spec.name.clone() });
                }
                None => {
                    seen.insert(*id, spec.name.clone());
                }
            }
        }
    }
    let mut total = Summary::default();
    for (spec, verdict) in specs.iter().zip(verdicts) {
        let dst = dest.join(&spec.name);
        let root = match verdict {
            Some(RootVerdict::WrongType(k)) => {
                let mut out = Emitter { summary: Summary::default(), sink: &mut *sink };
                out.emit(Record {
                    path: PathBuf::from(&spec.name),
                    outcome: Outcome::Mismatched,
                    kind: Kind::Dir,
                    code: Some(Code::VerifyMismatch),
                    detail: format!("source dir, destination {}", k.as_str()),
                });
                out.summary
            }
            Some(verdict) => {
                if matches!(verdict, RootVerdict::Absent) {
                    on_absent(&spec.name);
                }
                let mut reroot = |mut r: Record| {
                    r.path = if r.path == Path::new(".") {
                        PathBuf::from(&spec.name)
                    } else {
                        Path::new(&spec.name).join(&r.path)
                    };
                    sink(r);
                };
                let mut out = Emitter { summary: Summary::default(), sink: &mut reroot };
                walk_checked(
                    fs,
                    &spec.src,
                    &dst,
                    opts,
                    ControlFilter::NestedRoot,
                    verdict,
                    &mut out,
                );
                out.summary
            }
            // Invariant: no record precedes an exit 2, and `verify_file` has no refusal path today.
            None => match verify_file(fs, &spec.src, &dst, opts, sink) {
                Ok(s) => s,
                Err(_) => unreachable!("verify_file has no refusal path"),
            },
        };
        on_root_done(&spec.name, &root);
        total.matched += root.matched;
        total.missing += root.missing;
        total.mismatched += root.mismatched;
        total.extra += root.extra;
        total.unreadable += root.unreadable;
        total.not_entered += root.not_entered;
        total.excluded += root.excluded;
        total.bytes_hashed += root.bytes_hashed;
    }
    Ok(total)
}

/// What the root check decided for one source and destination pair.
#[derive(Debug)]
enum RootVerdict {
    /// The destination does not exist and is to be treated as empty.
    Absent,
    /// A root could not be examined: the root record `.` is reported, nothing is walked.
    Unreadable { side: &'static str, error: FsError },
    /// A folder root's destination exists but is not a directory (several sources only; never produced by
    /// `check_pair`).
    WrongType(Kind),
    /// The roots are distinct and both exist. `dest_identity` is the destination directory's identity when it is
    /// `Strong`, the only kind the alias rule trusts.
    Checked { dest_identity: Option<ObjectId> },
}

/// The root check of one pair (design 6.2): no directory is listed. `Err(reason)` is a refusal.
fn check_pair<F: DestinationRoot>(fs: &F, src: &Path, dst: &Path) -> Result<RootVerdict, String> {
    match check_roots(fs, src, dst) {
        Ok(Presence::Absent) => Ok(RootVerdict::Absent),
        Ok(Presence::Present(identity)) => Ok(RootVerdict::Checked {
            dest_identity: match identity {
                FileIdentity::Strong(id) => Some(id),
                FileIdentity::Weak(_) | FileIdentity::Unavailable => None,
            },
        }),
        Err(RootsCheck::Refused(reason)) => Err(reason),
        Err(RootsCheck::Unreadable(side, error)) => Ok(RootVerdict::Unreadable { side, error }),
    }
}

/// The walk of one checked pair (design 6.3), its records emitted through `out`.
fn walk_checked<F: DestinationRoot>(
    fs: &F,
    src: &Path,
    dst: &Path,
    opts: &VerifyOptions,
    filter: ControlFilter,
    verdict: RootVerdict,
    out: &mut Emitter<'_>,
) {
    let dst_absent = match verdict {
        RootVerdict::Unreadable { side, error } => {
            root_unreadable(out, &format!("{side}{}", error.source), error.code);
            return;
        }
        RootVerdict::Absent => true,
        RootVerdict::Checked { .. } => false,
        RootVerdict::WrongType(_) => {
            unreachable!("only verify_several probes the type, and it reports it itself")
        }
    };
    // The source walk is built first, then the destination's (a test pins the `read_dir` order).
    let mut a = match Side::new(
        fs,
        src,
        opts.mounts,
        opts.links == Links::Skip,
        ControlFilter::None,
        &opts.selection,
    ) {
        Ok(a) => a,
        Err(e) => {
            root_unreadable(out, &format!("{SOURCE}{}", e.source), e.code);
            return;
        }
    };
    let mut b = if dst_absent {
        Side::empty()
    } else {
        match Side::new(fs, dst, opts.mounts, false, filter, &opts.selection) {
            Ok(b) => b,
            Err(e) => {
                root_unreadable(out, &format!("{DESTINATION}{}", e.source), e.code);
                return;
            }
        }
    };
    let mut buf = vec![0u8; crate::copy::COPY_BUF_MAX];
    loop {
        let order = match (a.peek(), b.peek()) {
            (None, None) => break,
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (Some(x), Some(y)) => x.key.cmp(&y.key),
        };
        match order {
            Ordering::Less => {
                let x = a.take().expect("peeked");
                alone(out, &x, Outcome::Missing, &mut a, &mut b);
            }
            Ordering::Greater => {
                let y = b.take().expect("peeked");
                alone(out, &y, Outcome::Extra, &mut a, &mut b);
            }
            Ordering::Equal => {
                let x = a.take().expect("peeked");
                let y = b.take().expect("peeked");
                pair(fs, (src, dst), out, &mut buf, (&x, &y), (&mut a, &mut b));
            }
        }
    }
    // The SOURCE side only: the destination's walk drops entries the same way, but they are not the run's to count.
    out.summary.excluded += a.excluded();
}

/// Why the root check stopped the run.
enum RootsCheck {
    /// The roots are the same object or nested: the reason, for `VerifyError::RefusedRoots`.
    Refused(String),
    /// A root could not be examined: reported as the root record `.`, the text led by the root's name (`SOURCE` or
    /// `DESTINATION`).
    Unreadable(&'static str, FsError),
}

/// The detail prefix naming the source root, and the destination root.
const SOURCE: &str = "source: ";
const DESTINATION: &str = "destination: ";

const REFUSED_DST_IN_SRC: &str =
    "the destination is the source or lies inside it; nothing was compared";
const REFUSED_SRC_IN_DST: &str = "the source lies inside the destination; nothing was compared";
const BY_PATH_ALONE: &str = " (decided by path alone: a root's identity could not be confirmed)";

/// One containment direction of the root check (design 6.1). A refusal becomes `Refused(reason)`; any other failure is
/// `Unreadable`, led by the name of the root the failing query was about (`anchor.2` or `other.2`).
fn sided_check<F: DestinationRoot>(
    fs: &F,
    anchor: (&F::Dir, &Path, &'static str),
    other: (&F::Dir, &Path, &'static str),
    reason: &str,
    out: &mut crate::tree::TreeOutcome,
) -> Result<(), RootsCheck> {
    use crate::tree::{ContainmentTarget, containment_sided};
    match containment_sided(fs, anchor.0, anchor.1, other.0, other.1, Safety::Default, out) {
        Ok(()) => Ok(()),
        Err(f) if f.error.code() == Code::SafetyRejected => {
            Err(RootsCheck::Refused(reason.to_string()))
        }
        Err(f) => Err(RootsCheck::Unreadable(
            match f.target {
                ContainmentTarget::Anchor => anchor.2,
                ContainmentTarget::Other => other.2,
            },
            f.error.cause,
        )),
    }
}

/// What the root check found at the destination.
enum Presence {
    Absent,
    /// The destination's identity.
    Present(FileIdentity),
}

/// The root check (design 6.1), run before any directory is listed or file opened. `Presence::Absent` when the destination
/// does not exist and is to be treated as empty, else the destination's identity.
fn check_roots<F: DestinationRoot>(fs: &F, src: &Path, dst: &Path) -> Result<Presence, RootsCheck> {
    use crate::tree::{lexically_within, preflight};
    let d = match fs.destination_root(dst) {
        Ok(d) => d,
        Err(e) if e.source.kind() == std::io::ErrorKind::NotFound => {
            if lexically_within(dst, src) {
                return Err(RootsCheck::Refused(REFUSED_DST_IN_SRC.to_string()));
            }
            return Ok(Presence::Absent);
        }
        Err(e) => return Err(RootsCheck::Unreadable(DESTINATION, e)),
    };
    let s = fs.destination_root(src).map_err(|e| RootsCheck::Unreadable(SOURCE, e))?;
    let si = s.identity().map_err(|e| RootsCheck::Unreadable(SOURCE, e))?;
    let di = d.identity().map_err(|e| RootsCheck::Unreadable(DESTINATION, e))?;
    let mut out = crate::tree::TreeOutcome::default();
    preflight(si, di, Safety::Default, &mut out.warnings)
        .map_err(|_| RootsCheck::Refused(REFUSED_DST_IN_SRC.to_string()))?;
    sided_check(fs, (&d, dst, DESTINATION), (&s, src, SOURCE), REFUSED_DST_IN_SRC, &mut out)?;
    sided_check(fs, (&s, src, SOURCE), (&d, dst, DESTINATION), REFUSED_SRC_IN_DST, &mut out)?;
    if out.containment_degraded.is_some() {
        if lexically_within(dst, src) {
            return Err(RootsCheck::Refused(format!("{REFUSED_DST_IN_SRC}{BY_PATH_ALONE}")));
        }
        if lexically_within(src, dst) {
            return Err(RootsCheck::Refused(format!("{REFUSED_SRC_IN_DST}{BY_PATH_ALONE}")));
        }
    }
    Ok(Presence::Present(di))
}

/// The run ends at the root record `.` (design 6.6): one `unreadable`, nothing walked.
fn root_unreadable(out: &mut Emitter<'_>, detail: &str, code: Code) {
    out.unreadable(PathBuf::from("."), Kind::Dir, code, detail.to_string());
}

/// An item present on one side only: `missing` or `extra`. When its walk failed (both sides are then pruned): a
/// SOURCE-only one is `unreadable`; a DEST-only one is `extra` carrying the error's code and text, because nothing
/// the source holds can hide beneath it, so it never fails the run (design 6.2).
fn alone<F: FileSystem>(
    out: &mut Emitter<'_>,
    item: &Item,
    outcome: Outcome,
    a: &mut Side<'_, F>,
    b: &mut Side<'_, F>,
) {
    match (&item.failed, outcome) {
        (Some(e), Outcome::Extra) => {
            out.emit(Record {
                path: item.path.clone(),
                outcome: Outcome::Extra,
                kind: item.kind,
                code: Some(e.code),
                detail: e.source.to_string(),
            });
            a.prune(&item.path);
            b.prune(&item.path);
        }
        (Some(e), _) => {
            out.unreadable(item.path.clone(), item.kind, e.code, e.source.to_string());
            a.prune(&item.path);
            b.prune(&item.path);
        }
        (None, _) => out.alone(item, outcome),
    }
}

/// Two items with the same key.
fn pair<F: FileSystem>(
    fs: &F,
    roots: (&Path, &Path),
    out: &mut Emitter<'_>,
    buf: &mut [u8],
    items: (&Item, &Item),
    sides: (&mut Side<'_, F>, &mut Side<'_, F>),
) {
    let (x, y) = items;
    let (a, b) = sides;
    if let Some(e) = x.failed.as_ref().or(y.failed.as_ref()) {
        out.unreadable(x.path.clone(), x.kind, e.code, e.source.to_string());
        a.prune(&x.path);
        b.prune(&x.path);
        return;
    }
    match (x.kind, y.kind) {
        (Kind::Dir, Kind::Dir) => {
            let not_entered = x.not_entered || y.not_entered;
            let detail = if not_entered { MOUNT_NOT_ENTERED.to_string() } else { String::new() };
            out.emit(Record {
                path: x.path.clone(),
                outcome: Outcome::Match,
                kind: Kind::Dir,
                code: None,
                detail,
            });
            if not_entered {
                out.summary.not_entered += 1;
                a.prune(&x.path);
                b.prune(&x.path);
            }
        }
        (k1, k2) => {
            let (src, dst) = (roots.0.join(&x.path), roots.1.join(&x.path));
            compare_pair(fs, out, buf, &x.path, (k1, k2), (&src, &dst));
        }
    }
}

/// Two paired entries that are not both directories, by the design 6.3 table: shared by `verify_tree` and
/// `verify_file`. `path` is the record's path; `src` and `dst` are the entries to read.
fn compare_pair<F: FileSystem>(
    fs: &F,
    out: &mut Emitter<'_>,
    buf: &mut [u8],
    path: &Path,
    kinds: (Kind, Kind),
    paths: (&Path, &Path),
) {
    let (src, dst) = paths;
    match kinds {
        (Kind::File, Kind::File) => {
            let mut hashed = 0;
            let verdict = compare_files(fs, src, dst, buf, &mut hashed);
            out.summary.bytes_hashed += hashed;
            match verdict {
                Ok(None) => out.emit(record(path, Outcome::Match, Kind::File, None, "")),
                Ok(Some(detail)) => out.emit(Record {
                    path: path.to_path_buf(),
                    outcome: Outcome::Mismatched,
                    kind: Kind::File,
                    code: Some(Code::VerifyMismatch),
                    detail,
                }),
                Err((code, detail)) => out.unreadable(path.to_path_buf(), Kind::File, code, detail),
            }
        }
        (Kind::Symlink, Kind::Symlink) => {
            let payloads = fs.read_link(src).and_then(|s| Ok((s, fs.read_link(dst)?)));
            match payloads {
                Ok((s, d)) if s.payload.as_encoded_bytes() == d.payload.as_encoded_bytes() => {
                    out.emit(record(path, Outcome::Match, Kind::Symlink, None, ""));
                }
                Ok(_) => out.emit(Record {
                    path: path.to_path_buf(),
                    outcome: Outcome::Mismatched,
                    kind: Kind::Symlink,
                    code: Some(Code::VerifyMismatch),
                    detail: "symlink payloads differ".to_string(),
                }),
                Err(e) => {
                    out.unreadable(path.to_path_buf(), Kind::Symlink, e.code, e.source.to_string());
                }
            }
        }
        (Kind::Other, Kind::Other) => out.unreadable(
            path.to_path_buf(),
            Kind::Other,
            Code::SpecialFileUnsupported,
            SPECIAL_NOT_COMPARED.to_string(),
        ),
        (k1, k2) => out.emit(Record {
            path: path.to_path_buf(),
            outcome: Outcome::Mismatched,
            kind: k1,
            code: Some(Code::VerifyMismatch),
            detail: format!("source {}, destination {}", k1.as_str(), k2.as_str()),
        }),
    }
}

fn record(path: &Path, outcome: Outcome, kind: Kind, code: Option<Code>, detail: &str) -> Record {
    Record { path: path.to_path_buf(), outcome, kind, code, detail: detail.to_string() }
}

fn file_kind(t: FileType) -> Kind {
    match t {
        FileType::File => Kind::File,
        FileType::Dir => Kind::Dir,
        FileType::Symlink => Kind::Symlink,
        FileType::Other => Kind::Other,
    }
}

/// One source file against one destination path (the CLI has already applied section 4.1: `dst` is `DEST` or
/// `DEST/<name>`). Exactly one record, whose `path` is `dst.file_name()` (R4). No root check (design 6.1: "a FILE
/// verify is not subject"). `opts.links` is ignored: a symlink SOURCE never reaches the engine.
pub fn verify_file<F: FileSystem>(
    fs: &F,
    src: &Path,
    dst: &Path,
    _opts: &VerifyOptions,
    sink: &mut dyn FnMut(Record),
) -> Result<Summary, VerifyError> {
    let mut out = Emitter { summary: Summary::default(), sink: &mut *sink };
    let name = PathBuf::from(dst.file_name().unwrap_or(dst.as_os_str()));
    let src_type = match fs.metadata(src) {
        Ok(m) => file_kind(m.file_type),
        Err(e) => {
            out.unreadable(name, Kind::File, e.code, e.source.to_string());
            return Ok(out.summary);
        }
    };
    let dst_type = match fs.metadata(dst) {
        Ok(m) => file_kind(m.file_type),
        Err(e) if e.source.kind() == std::io::ErrorKind::NotFound => {
            out.emit(record(&name, Outcome::Missing, src_type, None, ""));
            return Ok(out.summary);
        }
        Err(e) => {
            out.unreadable(name, src_type, e.code, e.source.to_string());
            return Ok(out.summary);
        }
    };
    let mut buf = vec![0u8; crate::copy::COPY_BUF_MAX];
    compare_pair(fs, &mut out, &mut buf, &name, (src_type, dst_type), (src, dst));
    Ok(out.summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::copy::COPY_BUF_MAX;
    use crate::fault_fs::FaultFs;
    use flux_fs::{LinkKind, MountRoot};

    fn opts() -> VerifyOptions {
        VerifyOptions {
            links: Links::Copy,
            mounts: MountPolicy::Cross,
            hash: HashAlgorithm::Blake3,
            selection: flux_fs::Selection::default(),
        }
    }

    /// `fs.create_dir` for every directory (the fake lists only what `create_dir` made, `fault_fs.rs:1782-1809`),
    /// parents first, then the files.
    fn tree(fs: &FaultFs, root: &str, dirs: &[&str], files: &[(&str, &[u8])]) {
        fs.create_dir(Path::new(root)).unwrap();
        for d in dirs {
            fs.create_dir(&Path::new(root).join(d)).unwrap();
        }
        for (f, bytes) in files {
            fs.write_file(Path::new(root).join(f), bytes);
        }
    }

    type Row = (String, Outcome, Kind, Option<Code>, String);

    fn row(r: &Record) -> Row {
        let path = r
            .path
            .components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        (path, r.outcome, r.kind, r.code, r.detail.clone())
    }

    fn run(fs: &FaultFs, src: &str, dst: &str, o: &VerifyOptions) -> (Vec<Row>, Summary) {
        let mut rows = Vec::new();
        let s = verify_tree(fs, Path::new(src), Path::new(dst), o, &mut |r| rows.push(row(&r)))
            .unwrap();
        (rows, s)
    }

    /// Design test 11: every logged call is a READ.
    const READS: &[&str] = &[
        "metadata(",
        "read_dir(",
        "open_read(",
        "read_link(",
        "source_mount_root(",
        "canonical_path(",
        "identity(",
    ];

    /// `before` is the call count after the fixture was built: setup calls (`create_dir`) are not the run's.
    fn assert_no_writes(fs: &FaultFs, before: usize) {
        for c in fs.calls().into_iter().skip(before) {
            assert!(READS.iter().any(|r| c.starts_with(r)), "{c}");
        }
    }

    fn rec(path: &str, outcome: Outcome, kind: Kind) -> Row {
        (path.to_string(), outcome, kind, None, String::new())
    }

    #[test]
    fn identical_trees_match_and_exit_clean() {
        let fs = FaultFs::new();
        for root in ["/s", "/d"] {
            tree(&fs, root, &["sub"], &[("a", b"alpha"), ("sub/b", b"bravo!!")]);
        }
        let before = fs.calls().len();
        let (rows, s) = run(&fs, "/s", "/d", &opts());
        assert_eq!(
            rows,
            vec![
                rec("a", Outcome::Match, Kind::File),
                rec("sub", Outcome::Match, Kind::Dir),
                rec("sub/b", Outcome::Match, Kind::File),
            ]
        );
        assert_eq!(s, Summary { matched: 3, bytes_hashed: 2 * (5 + 7), ..Summary::default() });
        assert!(!s.failed());
        assert_no_writes(&fs, before);
    }

    #[test]
    fn a_byte_in_the_last_buffer_is_a_mismatch() {
        let fs = FaultFs::new();
        let mut a = vec![7u8; COPY_BUF_MAX + 5];
        let mut b = a.clone();
        *b.last_mut().unwrap() = 8;
        tree(&fs, "/s", &[], &[("big", &a)]);
        tree(&fs, "/d", &[], &[("big", &b)]);
        a.clear();
        let (rows, s) = run(&fs, "/s", "/d", &opts());
        assert_eq!(rows.len(), 1);
        let (path, outcome, kind, code, detail) = &rows[0];
        assert_eq!(
            (path.as_str(), *outcome, *kind, *code),
            ("big", Outcome::Mismatched, Kind::File, Some(Code::VerifyMismatch))
        );
        let parts: Vec<&str> = detail.split(" != ").collect();
        assert_eq!(parts.len(), 2, "{detail}");
        for p in parts {
            assert_eq!(p.len(), 8, "{detail}");
            assert!(p.chars().all(|c| c.is_ascii_hexdigit()), "{detail}");
        }
        assert_eq!(s.mismatched, 1);
        assert!(s.failed());
    }

    #[test]
    fn missing_and_extra_files_and_directories() {
        let fs = FaultFs::new();
        tree(&fs, "/s", &["md"], &[("m", b"m"), ("md/x", b"x")]);
        tree(&fs, "/d", &["ed"], &[("e", b"e"), ("ed/y", b"y")]);
        let (rows, s) = run(&fs, "/s", "/d", &opts());
        assert_eq!(
            rows,
            vec![
                rec("e", Outcome::Extra, Kind::File),
                rec("ed", Outcome::Extra, Kind::Dir),
                rec("ed/y", Outcome::Extra, Kind::File),
                rec("m", Outcome::Missing, Kind::File),
                rec("md", Outcome::Missing, Kind::Dir),
                rec("md/x", Outcome::Missing, Kind::File),
            ]
        );
        assert_eq!((s.extra, s.missing), (3, 3));
        assert!(!fs.called("open_read(/s/md/x)"));
        assert!(!fs.called("open_read(/d/ed/y)"));
        assert!(!fs.called("open_read("));
    }

    #[test]
    fn a_type_mismatch_drains_the_children_as_extra_or_missing() {
        let fs = FaultFs::new();
        tree(&fs, "/s", &["a"], &[("a/x", b"x"), ("b", b"b")]);
        tree(&fs, "/d", &["b"], &[("a", b"a"), ("b/y", b"y")]);
        let (rows, _) = run(&fs, "/s", "/d", &opts());
        let mm = |p: &str, k: Kind, d: &str| {
            (p.to_string(), Outcome::Mismatched, k, Some(Code::VerifyMismatch), d.to_string())
        };
        assert_eq!(
            rows,
            vec![
                mm("a", Kind::Dir, "source dir, destination file"),
                rec("a/x", Outcome::Missing, Kind::File),
                mm("b", Kind::File, "source file, destination dir"),
                rec("b/y", Outcome::Extra, Kind::File),
            ]
        );
        assert!(!fs.called("open_read("));
    }

    #[test]
    fn component_wise_order_is_not_a_bytewise_sort() {
        let fs = FaultFs::new();
        tree(&fs, "/s", &["a"], &[("a/b", b"1"), ("a.txt", b"2"), ("a-b", b"3")]);
        tree(&fs, "/d", &["a"], &[("a.txt", b"2"), ("a-b", b"3")]);
        let (rows, _) = run(&fs, "/s", "/d", &opts());
        // `-` (0x2d) sorts before `.` (0x2e) at the root, `walk.rs:161-163`, so `a-b` precedes `a.txt`.
        assert_eq!(
            rows,
            vec![
                rec("a", Outcome::Match, Kind::Dir),
                rec("a/b", Outcome::Missing, Kind::File),
                rec("a-b", Outcome::Match, Kind::File),
                rec("a.txt", Outcome::Match, Kind::File),
            ]
        );
    }

    #[test]
    fn records_stream_before_the_walk_is_exhausted() {
        let fs = FaultFs::new();
        tree(&fs, "/s", &["z"], &[("a", b"a"), ("z/q", b"q")]);
        tree(&fs, "/d", &[], &[("a", b"a")]);
        let mut first = true;
        let mut n = 0;
        let s = verify_tree(&fs, Path::new("/s"), Path::new("/d"), &opts(), &mut |_| {
            n += 1;
            if first {
                first = false;
                assert!(!fs.called("read_dir(/s/z)"));
            }
        })
        .unwrap();
        assert_eq!(n, 3);
        assert_eq!(s.matched, 1);
        assert!(fs.called("read_dir(/s/z)"));
    }

    #[test]
    fn empty_files_match_and_an_empty_against_a_byte_is_read_on_both_sides() {
        let fs = FaultFs::new();
        tree(&fs, "/s", &[], &[("e", b""), ("f", b"")]);
        tree(&fs, "/d", &[], &[("e", b""), ("f", b"x")]);
        let (rows, _) = run(&fs, "/s", "/d", &opts());
        assert_eq!(rows[0], rec("e", Outcome::Match, Kind::File));
        assert_eq!(rows[1].0, "f");
        assert_eq!(rows[1].1, Outcome::Mismatched);
        assert!(fs.called("open_read(/d/f)"));
        assert!(fs.called("open_read(/s/f)"));
    }

    #[test]
    fn two_empty_trees_give_no_records() {
        let fs = FaultFs::new();
        tree(&fs, "/s", &[], &[]);
        tree(&fs, "/d", &[], &[]);
        let (rows, s) = run(&fs, "/s", "/d", &opts());
        assert!(rows.is_empty());
        assert_eq!(s, Summary::default());
        assert!(!fs.called("open_read("));
    }

    const ZEROS: &str = "00000000000000000000000000000000";
    /// 32 hex characters, but not lowercase: not an operation id, so user data.
    const UPPER: &str = "ABCDEFABCDEFABCDEFABCDEFABCDEFAB";

    fn mm(p: &str, k: Kind, d: &str) -> Row {
        (p.to_string(), Outcome::Mismatched, k, Some(Code::VerifyMismatch), d.to_string())
    }

    #[test]
    fn symlink_payloads_equal_differ_and_against_a_file() {
        let fs = FaultFs::new();
        tree(&fs, "/s", &[], &[]);
        tree(&fs, "/d", &[], &[("l3", b"x")]);
        fs.add_symlink_to("/s/l1", "t", LinkKind::Unspecified);
        fs.add_symlink_to("/d/l1", "t", LinkKind::Unspecified);
        fs.add_symlink_to("/s/l2", "a", LinkKind::Unspecified);
        fs.add_symlink_to("/d/l2", "b", LinkKind::Unspecified);
        fs.add_symlink_to("/s/l3", "t", LinkKind::Unspecified);
        let (rows, s) = run(&fs, "/s", "/d", &opts());
        assert_eq!(
            rows,
            vec![
                rec("l1", Outcome::Match, Kind::Symlink),
                mm("l2", Kind::Symlink, "symlink payloads differ"),
                mm("l3", Kind::Symlink, "source symlink, destination file"),
            ]
        );
        assert_eq!((s.matched, s.mismatched, s.bytes_hashed), (1, 2, 0));
        assert!(!fs.called("open_read("));
    }

    #[test]
    fn a_symlink_whose_payload_cannot_be_read_is_unreadable_and_the_other_side_is_not_read() {
        for (nth, dst_read) in [(1, false), (2, true)] {
            let fs = FaultFs::new();
            tree(&fs, "/s", &[], &[]);
            tree(&fs, "/d", &[], &[]);
            fs.add_symlink_to("/s/l", "t", LinkKind::Unspecified);
            fs.add_symlink_to("/d/l", "t", LinkKind::Unspecified);
            // The walker never calls read_link; compare_pair reads the source link first (call 1), then the
            // destination link (call 2).
            fs.fail_nth(
                "read_link",
                nth,
                Code::PermissionDenied,
                std::io::ErrorKind::PermissionDenied,
            );
            let (rows, s) = run(&fs, "/s", "/d", &opts());
            assert_eq!(rows.len(), 1, "nth {nth}: {rows:?}");
            assert_eq!(
                (rows[0].0.as_str(), rows[0].1, rows[0].2, rows[0].3),
                ("l", Outcome::Unreadable, Kind::Symlink, Some(Code::PermissionDenied)),
                "nth {nth}"
            );
            assert!(!rows[0].4.is_empty(), "nth {nth}");
            assert!(fs.called("read_link(/s/l)"), "nth {nth}");
            assert_eq!(fs.called("read_link(/d/l)"), dst_read, "nth {nth}");
            assert_eq!((s.unreadable, s.mismatched, s.matched), (1, 0, 0), "nth {nth}");
        }
    }

    #[test]
    fn links_skip_drops_source_symlinks_and_keeps_a_dest_only_one_as_extra() {
        let fs = FaultFs::new();
        tree(&fs, "/s", &[], &[("f", b"x")]);
        tree(&fs, "/d", &[], &[]);
        fs.add_symlink_to("/s/s", "t", LinkKind::Unspecified);
        fs.add_symlink_to("/d/s", "t", LinkKind::Unspecified);
        fs.add_symlink_to("/d/d", "t", LinkKind::Unspecified);
        fs.add_symlink_to("/d/f", "t", LinkKind::Unspecified);
        let o = VerifyOptions { links: Links::Skip, ..opts() };
        let (rows, s) = run(&fs, "/s", "/d", &o);
        assert_eq!(
            rows,
            vec![
                rec("d", Outcome::Extra, Kind::Symlink),
                mm("f", Kind::File, "source file, destination symlink"),
                rec("s", Outcome::Extra, Kind::Symlink),
            ]
        );
        assert_eq!((s.extra, s.mismatched, s.matched), (2, 1, 0));
        assert!(!fs.called("read_link("));
    }

    #[test]
    fn special_files_are_never_opened() {
        let fs = FaultFs::new();
        tree(&fs, "/s", &[], &[]);
        tree(&fs, "/d", &[], &[("o2", b"x")]);
        for p in ["/s/o1", "/d/o1", "/s/o2", "/s/o3", "/d/o4"] {
            fs.add_special(p);
        }
        let (rows, s) = run(&fs, "/s", "/d", &opts());
        assert_eq!(
            rows,
            vec![
                (
                    "o1".to_string(),
                    Outcome::Unreadable,
                    Kind::Other,
                    Some(Code::SpecialFileUnsupported),
                    SPECIAL_NOT_COMPARED.to_string()
                ),
                mm("o2", Kind::Other, "source other, destination file"),
                rec("o3", Outcome::Missing, Kind::Other),
                rec("o4", Outcome::Extra, Kind::Other),
            ]
        );
        assert_eq!((s.unreadable, s.mismatched, s.missing, s.extra), (1, 1, 1, 1));
        assert!(!fs.calls().iter().any(|c| c.starts_with("open_read(") && c.contains("/o")));
    }

    #[test]
    fn an_unreadable_file_on_either_side_is_unreadable_and_the_other_side_is_not_guessed() {
        let fs = FaultFs::new();
        tree(&fs, "/s", &[], &[("a", b"aaaa"), ("b", b"bbb"), ("c", b"cc")]);
        tree(&fs, "/d", &[], &[("a", b"aaaa"), ("b", b"bbb"), ("c", b"cc")]);
        fs.fail_read_nth("/s/a", 1, std::io::ErrorKind::Other);
        // open_read calls: /s/a (1, its read then fails), /s/b (2), /d/b (3).
        fs.fail_nth("open_read", 3, Code::PermissionDenied, std::io::ErrorKind::PermissionDenied);
        let (rows, s) = run(&fs, "/s", "/d", &opts());
        let classified = FsError::classify(&std::io::Error::from(std::io::ErrorKind::Other));
        assert_eq!(rows.len(), 3);
        assert_eq!(
            (rows[0].0.as_str(), rows[0].1, rows[0].2, rows[0].3),
            ("a", Outcome::Unreadable, Kind::File, Some(classified))
        );
        assert!(!rows[0].4.is_empty());
        assert_eq!(
            (rows[1].0.as_str(), rows[1].1, rows[1].2, rows[1].3),
            ("b", Outcome::Unreadable, Kind::File, Some(Code::PermissionDenied))
        );
        assert_eq!(rows[2], rec("c", Outcome::Match, Kind::File));
        assert!(!fs.called("open_read(/d/a)"));
        assert_eq!(s.bytes_hashed, 3 + 2 * 2);
        assert_eq!((s.unreadable, s.matched), (2, 1));
    }

    #[test]
    fn an_unreadable_directory_prunes_both_sides_and_siblings_continue() {
        let fs = FaultFs::new();
        tree(&fs, "/s", &["bad", "good"], &[("bad/x", b"x"), ("good/y", b"y")]);
        tree(&fs, "/d", &["bad", "good"], &[("bad/z", b"z"), ("good/y", b"y")]);
        // read_dir(/s), read_dir(/d), then read_dir(/s/bad): the source head is pulled first.
        fs.fail_nth("read_dir", 3, Code::PermissionDenied, std::io::ErrorKind::PermissionDenied);
        let (rows, s) = run(&fs, "/s", "/d", &opts());
        assert!(fs.called("read_dir(/s/bad)"));
        assert_eq!(rows.len(), 3, "{rows:?}");
        assert_eq!(
            (rows[0].0.as_str(), rows[0].1, rows[0].2, rows[0].3),
            ("bad", Outcome::Unreadable, Kind::Dir, Some(Code::PermissionDenied))
        );
        assert_eq!(rows[1], rec("good", Outcome::Match, Kind::Dir));
        assert_eq!(rows[2], rec("good/y", Outcome::Match, Kind::File));
        assert_eq!((s.unreadable, s.matched), (1, 2));
    }

    /// The `read_dir` calls so far, in order: proves WHICH call a `fail_nth("read_dir", n, ..)` hit.
    fn read_dirs(fs: &FaultFs) -> Vec<String> {
        fs.calls().into_iter().filter(|c| c.starts_with("read_dir(")).collect()
    }

    #[test]
    fn a_destination_only_directory_that_cannot_be_listed_is_extra_with_its_error_and_never_fails()
    {
        let fs = FaultFs::new();
        tree(&fs, "/s", &[], &[("a", b"a")]);
        tree(&fs, "/d", &["lost"], &[("a", b"a"), ("lost/x", b"x"), ("z", b"z")]);
        // read_dir(/s) (1), read_dir(/d) (2), then the DEST-only `lost` once `a` is paired (3).
        fs.fail_nth("read_dir", 3, Code::PermissionDenied, std::io::ErrorKind::PermissionDenied);
        let (rows, s) = run(&fs, "/s", "/d", &opts());
        assert_eq!(read_dirs(&fs)[2], "read_dir(/d/lost)");
        assert_eq!(rows.len(), 3, "{rows:?}");
        assert_eq!(rows[0], rec("a", Outcome::Match, Kind::File));
        assert_eq!(
            (rows[1].0.as_str(), rows[1].1, rows[1].2, rows[1].3),
            ("lost", Outcome::Extra, Kind::Dir, Some(Code::PermissionDenied))
        );
        assert!(rows[1].4.contains("injected"), "{}", rows[1].4);
        assert_eq!(rows[2], rec("z", Outcome::Extra, Kind::File));
        assert_eq!((s.extra, s.unreadable, s.matched), (2, 0, 1));
        assert!(!s.failed());
    }

    #[test]
    fn the_same_failure_on_a_directory_both_sides_hold_stays_unreadable() {
        let fs = FaultFs::new();
        tree(&fs, "/s", &["lost"], &[("a", b"a"), ("lost/x", b"x")]);
        tree(&fs, "/d", &["lost"], &[("a", b"a"), ("lost/y", b"y")]);
        // read_dir(/s) (1), read_dir(/d) (2), read_dir(/s/lost) (3), read_dir(/d/lost) (4).
        fs.fail_nth("read_dir", 4, Code::PermissionDenied, std::io::ErrorKind::PermissionDenied);
        let (rows, s) = run(&fs, "/s", "/d", &opts());
        assert_eq!(read_dirs(&fs)[3], "read_dir(/d/lost)");
        assert_eq!(rows.len(), 2, "{rows:?}");
        assert_eq!(
            (rows[1].0.as_str(), rows[1].1, rows[1].2, rows[1].3),
            ("lost", Outcome::Unreadable, Kind::Dir, Some(Code::PermissionDenied))
        );
        assert_eq!((s.unreadable, s.extra, s.missing), (1, 0, 0));
        assert!(s.failed());
    }

    #[test]
    fn a_source_only_directory_that_cannot_be_listed_stays_unreadable() {
        let fs = FaultFs::new();
        tree(&fs, "/s", &["lost"], &[("a", b"a"), ("lost/x", b"x"), ("z", b"z")]);
        tree(&fs, "/d", &[], &[("a", b"a"), ("z", b"z")]);
        // read_dir(/s) (1), read_dir(/d) (2), then the SOURCE-only `lost` (3).
        fs.fail_nth("read_dir", 3, Code::PermissionDenied, std::io::ErrorKind::PermissionDenied);
        let (rows, s) = run(&fs, "/s", "/d", &opts());
        assert_eq!(read_dirs(&fs)[2], "read_dir(/s/lost)");
        assert_eq!(rows.len(), 3, "{rows:?}");
        assert_eq!(
            (rows[1].0.as_str(), rows[1].1, rows[1].2, rows[1].3),
            ("lost", Outcome::Unreadable, Kind::Dir, Some(Code::PermissionDenied))
        );
        assert_eq!(rows[2], rec("z", Outcome::Match, Kind::File));
        assert_eq!((s.unreadable, s.extra, s.missing), (1, 0, 0));
        assert!(s.failed());
    }

    #[test]
    fn names_differing_only_in_case_are_a_missing_and_an_extra_by_their_bytes() {
        let fs = FaultFs::new();
        tree(&fs, "/s", &[], &[("README", b"r")]);
        tree(&fs, "/d", &[], &[("Readme", b"r")]);
        let (rows, s) = run(&fs, "/s", "/d", &opts());
        // 'E' (0x45) sorts before 'e' (0x65), so the source name comes first.
        assert_eq!(
            rows,
            vec![
                rec("README", Outcome::Missing, Kind::File),
                rec("Readme", Outcome::Extra, Kind::File)
            ]
        );
        assert_eq!((s.missing, s.extra, s.matched), (1, 1, 0));
        assert!(s.failed());
        assert!(!fs.called("open_read("));
    }

    #[test]
    fn a_users_dot_flux_inside_a_nested_root_is_compared_and_a_partial_temporary_is_hidden() {
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/s")).unwrap();
        fs.create_dir(Path::new("/d")).unwrap();
        tree(&fs, "/s/a", &[".flux"], &[(".flux/x", b"x"), (".flux-root.lock", b"l"), ("f", b"f")]);
        let partial = format!("f.flux-partial.{ZEROS}");
        tree(
            &fs,
            "/d/a",
            &[".flux"],
            &[(".flux/x", b"x"), (".flux-root.lock", b"l"), ("f", b"f"), (partial.as_str(), b"p")],
        );
        let mut rows = Vec::new();
        let s = verify_tree_filtered(
            &fs,
            Path::new("/s/a"),
            Path::new("/d/a"),
            &opts(),
            ControlFilter::NestedRoot,
            &mut || {},
            &mut |r| rows.push(row(&r)),
        )
        .unwrap();
        assert_eq!(
            rows,
            vec![
                rec(".flux", Outcome::Match, Kind::Dir),
                rec(".flux/x", Outcome::Match, Kind::File),
                rec(".flux-root.lock", Outcome::Match, Kind::File),
                rec("f", Outcome::Match, Kind::File),
            ]
        );
        assert_eq!((s.matched, s.extra, s.missing), (4, 0, 0));
    }

    #[test]
    fn control_state_at_the_destination_root_is_never_reported() {
        let fs = FaultFs::new();
        tree(&fs, "/s", &["sub"], &[]);
        let ops = format!(".flux/operations/{ZEROS}");
        tree(
            &fs,
            "/d",
            &["sub", ".flux", ".flux/operations", ops.as_str()],
            &[
                (format!("{ops}/state").as_str(), b"s"),
                (".flux-root.lock", b""),
                (format!("a.flux-partial.{ZEROS}").as_str(), b"p"),
                (".flux-partial.txt", b"u"),
                ("x.flux-partial.1", b"u"),
                ("sub/.flux", b"u"),
                ("sub/.flux-root.lock", b"u"),
                (format!("a.flux-partial.{UPPER}").as_str(), b"u"),
                (format!("sub/b.flux-partial.{ZEROS}").as_str(), b"p"),
            ],
        );
        fs.add_symlink_to(format!("/d/l.flux-partial.{ZEROS}"), "t", LinkKind::Unspecified);
        let (rows, s) = run(&fs, "/s", "/d", &opts());
        assert_eq!(
            rows,
            vec![
                rec(".flux-partial.txt", Outcome::Extra, Kind::File),
                rec(format!("a.flux-partial.{UPPER}").as_str(), Outcome::Extra, Kind::File),
                rec("sub", Outcome::Match, Kind::Dir),
                rec("sub/.flux", Outcome::Extra, Kind::File),
                rec("sub/.flux-root.lock", Outcome::Extra, Kind::File),
                rec("x.flux-partial.1", Outcome::Extra, Kind::File),
            ]
        );
        assert_eq!((s.extra, s.matched), (5, 1));
        // The brief also asked for `!fs.called("read_dir(/d/.flux")`. `Walk::enter` lists a directory before it yields
        // the `Dir` event (`walk.rs:452`), so the subtree is dropped from the records but is still listed; asserting
        // otherwise needs a walker change (reported, not bent here).
    }

    #[test]
    fn a_top_level_dot_flux_file_at_the_destination_is_user_data() {
        let fs = FaultFs::new();
        tree(&fs, "/s", &[], &[]);
        tree(&fs, "/d", &[], &[(".flux", b"u")]);
        let (rows, s) = run(&fs, "/s", "/d", &opts());
        assert_eq!(rows, vec![rec(".flux", Outcome::Extra, Kind::File)]);
        assert_eq!(s.extra, 1);
    }

    #[test]
    fn a_source_dot_flux_directory_is_missing_when_dest_has_only_control_state() {
        let fs = FaultFs::new();
        tree(&fs, "/s", &[".flux"], &[(".flux/data", b"x")]);
        let ops = format!(".flux/operations/{ZEROS}");
        tree(&fs, "/d", &[".flux", ".flux/operations", ops.as_str()], &[]);
        let (rows, s) = run(&fs, "/s", "/d", &opts());
        assert_eq!(
            rows,
            vec![
                rec(".flux", Outcome::Missing, Kind::Dir),
                rec(".flux/data", Outcome::Missing, Kind::File),
            ]
        );
        assert_eq!((s.missing, s.extra), (2, 0));
    }

    #[test]
    fn an_unlistable_directory_inside_destination_control_state_is_still_ignored() {
        // `Walk::enter` lists a directory before it yields its `Dir` event (`walk.rs:452`), so a listing fault
        // on or under `/d/.flux` reaches the merge as a `WalkError`, which the control-state filter must drop
        // exactly like the directory it names (test-audit gap CA-1).
        for faulted in [".flux", ".flux/operations"] {
            let fs = FaultFs::new();
            tree(&fs, "/s", &["sub"], &[("sub/f", b"x")]);
            tree(&fs, "/d", &["sub", ".flux", ".flux/operations"], &[("sub/f", b"x")]);
            fs.fail_read_dir_of(
                Path::new("/d").join(faulted),
                Code::PermissionDenied,
                std::io::ErrorKind::PermissionDenied,
            );
            let (rows, s) = run(&fs, "/s", "/d", &opts());
            assert!(
                fs.called(&format!("read_dir(/d/{faulted})")),
                "{faulted}: the fault was not reached"
            );
            assert_eq!(
                rows,
                vec![
                    rec("sub", Outcome::Match, Kind::Dir),
                    rec("sub/f", Outcome::Match, Kind::File)
                ],
                "{faulted}"
            );
            assert_eq!((s.matched, s.extra, s.unreadable), (2, 0, 0), "{faulted}");
        }
    }

    #[test]
    fn a_directory_named_like_control_state_and_a_source_side_control_name_are_user_data() {
        // The lock name hides only a top-level FILE and the partial pattern only a file or symlink (6.4); a
        // directory under either name is user data, and the source walker never filters (test-audit gap AB-2).
        let fs = FaultFs::new();
        let a = format!("a.flux-partial.{ZEROS}");
        let b = format!("b.flux-partial.{ZEROS}");
        let c = format!("sub/c.flux-partial.{ZEROS}");
        tree(&fs, "/s", &["sub"], &[(".flux-root.lock", b"s"), (b.as_str(), b"s")]);
        tree(&fs, "/d", &[".flux-root.lock", a.as_str(), "sub", c.as_str()], &[]);
        let (rows, s) = run(&fs, "/s", "/d", &opts());
        assert_eq!(
            rows,
            vec![
                mm(".flux-root.lock", Kind::File, "source file, destination dir"),
                rec(a.as_str(), Outcome::Extra, Kind::Dir),
                rec(b.as_str(), Outcome::Missing, Kind::File),
                rec("sub", Outcome::Match, Kind::Dir),
                rec(c.as_str(), Outcome::Extra, Kind::Dir),
            ]
        );
        assert_eq!((s.mismatched, s.extra, s.missing, s.matched), (1, 2, 1, 1));
    }

    const REFUSE_A: &str = "the destination is the source or lies inside it; nothing was compared";
    const REFUSE_B: &str = "the source lies inside the destination; nothing was compared";
    const DEGRADED: &str = " (decided by path alone: a root's identity could not be confirmed)";

    /// Run a verify that must be refused; no directory was listed and no file opened.
    fn refused(fs: &FaultFs, src: &str, dst: &str) -> String {
        let mut records = 0;
        let r = verify_tree(fs, Path::new(src), Path::new(dst), &opts(), &mut |_| records += 1);
        let Err(VerifyError::RefusedRoots { reason }) = r else {
            panic!("expected a refusal, got {r:?} for {src} {dst}")
        };
        assert_eq!(records, 0);
        assert!(!fs.called("read_dir("));
        assert!(!fs.called("open_read("));
        reason
    }

    #[test]
    fn dest_equal_to_source_is_refused_before_any_read() {
        let fs = FaultFs::new();
        tree(&fs, "/s", &[], &[("a", b"x")]);
        assert_eq!(refused(&fs, "/s", "/s"), REFUSE_A);
    }

    #[test]
    fn dest_inside_source_and_source_inside_dest_are_refused() {
        let fs = FaultFs::new();
        tree(&fs, "/s", &["inner"], &[("a", b"x")]);
        assert_eq!(refused(&fs, "/s", "/s/inner"), REFUSE_A);
        assert_eq!(refused(&fs, "/s/inner", "/s"), REFUSE_B);
    }

    #[test]
    fn an_alias_of_the_source_is_refused_by_path_or_by_identity() {
        let fs = FaultFs::new();
        tree(&fs, "/s", &[], &[("a", b"x")]);
        tree(&fs, "/alias", &[], &[]);
        fs.set_canonical_path("/alias", "/s");
        // Without this the fake's minted identity for /alias differs from /s's, and `canonical_of` rightly fails with
        // "the path reported for an open directory now holds a different object" (`tree.rs:4333`) before the lexical
        // test runs. An alias whose identity is not Strong is the case this branch exists for.
        fs.set_identity("/alias", flux_fs::FileIdentity::Unavailable);
        assert_eq!(refused(&fs, "/s", "/alias"), REFUSE_A);

        let fs = FaultFs::new();
        tree(&fs, "/s", &[], &[("a", b"x")]);
        tree(&fs, "/alias2", &[], &[]);
        let id = fs.metadata(Path::new("/s")).unwrap().identity;
        fs.set_identity("/alias2", id);
        assert_eq!(refused(&fs, "/s", "/alias2"), REFUSE_A);
    }

    #[test]
    fn a_degraded_root_falls_back_to_the_lexical_test_and_says_so() {
        let unsupported = std::io::ErrorKind::Unsupported;
        let fs = FaultFs::new();
        tree(&fs, "/s", &["inner"], &[]);
        fs.fail_canonical_path_of("/s/inner", Code::IoError, unsupported);
        assert_eq!(refused(&fs, "/s", "/s/inner"), format!("{REFUSE_A}{DEGRADED}"));

        // The other direction: the source lies inside the destination, decided by path alone.
        let fs = FaultFs::new();
        tree(&fs, "/s", &["inner"], &[]);
        fs.fail_canonical_path_of("/s", Code::IoError, unsupported);
        assert_eq!(refused(&fs, "/s/inner", "/s"), format!("{REFUSE_B}{DEGRADED}"));

        let fs = FaultFs::new();
        tree(&fs, "/s", &[], &[("a", b"x")]);
        tree(&fs, "/d", &[], &[("a", b"x")]);
        fs.fail_canonical_path_of("/d", Code::IoError, unsupported);
        let (rows, s) = run(&fs, "/s", "/d", &opts());
        assert_eq!(rows, vec![rec("a", Outcome::Match, Kind::File)]);
        assert_eq!(s.matched, 1);
        assert!(!s.failed());
    }

    #[test]
    fn siblings_with_a_common_prefix_are_not_refused() {
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/d")).unwrap();
        tree(&fs, "/d/a", &[], &[("f", b"x")]);
        tree(&fs, "/d/ab", &[], &[("f", b"x")]);
        let (rows, s) = run(&fs, "/d/a", "/d/ab", &opts());
        assert_eq!(rows, vec![rec("f", Outcome::Match, Kind::File)]);
        assert_eq!(s.matched, 1);
    }

    #[test]
    fn an_absent_destination_is_an_empty_one_and_nothing_is_created() {
        let fs = FaultFs::new();
        tree(&fs, "/s", &["sub"], &[("a", b"x"), ("sub/b", b"y")]);
        let before = fs.calls().len();
        let (rows, s) = run(&fs, "/s", "/nope", &opts());
        assert_eq!(
            rows,
            vec![
                rec("a", Outcome::Missing, Kind::File),
                rec("sub", Outcome::Missing, Kind::Dir),
                rec("sub/b", Outcome::Missing, Kind::File),
            ]
        );
        assert_eq!(s.missing, 3);
        assert!(s.failed());
        assert_no_writes(&fs, before);
        assert!(!fs.exists("/nope"));
    }

    #[test]
    fn an_absent_destination_inside_the_source_is_still_refused() {
        let fs = FaultFs::new();
        tree(&fs, "/s", &[], &[("a", b"x")]);
        assert_eq!(refused(&fs, "/s", "/s/nope"), REFUSE_A);
    }

    #[test]
    fn an_unlistable_destination_root_is_one_unreadable_record() {
        let fs = FaultFs::new();
        tree(&fs, "/s", &[], &[("a", b"x")]);
        tree(&fs, "/d", &[], &[("a", b"x")]);
        // The source walk is built first (read_dir 1); the root check lists nothing, so read_dir(/d) is the 2nd.
        fs.fail_nth("read_dir", 2, Code::PermissionDenied, std::io::ErrorKind::PermissionDenied);
        let (rows, s) = run(&fs, "/s", "/d", &opts());
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(
            (rows[0].0.as_str(), rows[0].1, rows[0].2, rows[0].3),
            (".", Outcome::Unreadable, Kind::Dir, Some(Code::PermissionDenied))
        );
        assert!(rows[0].4.starts_with("destination: "), "{}", rows[0].4);
        assert!(fs.called("read_dir(/d)"));
        assert_eq!(s.unreadable, 1);
        assert!(!fs.called("open_read("));
    }

    #[test]
    fn an_unlistable_source_root_is_one_unreadable_record() {
        let fs = FaultFs::new();
        tree(&fs, "/s", &[], &[("a", b"x")]);
        tree(&fs, "/d", &[], &[("a", b"x")]);
        // The source walk is built first, so read_dir(/s) is the 1st read_dir (the root check lists nothing).
        fs.fail_nth("read_dir", 1, Code::PermissionDenied, std::io::ErrorKind::PermissionDenied);
        let (rows, s) = run(&fs, "/s", "/d", &opts());
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(
            (rows[0].0.as_str(), rows[0].1, rows[0].2, rows[0].3),
            (".", Outcome::Unreadable, Kind::Dir, Some(Code::PermissionDenied))
        );
        assert!(rows[0].4.starts_with("source: "), "{}", rows[0].4);
        assert!(fs.called("read_dir(/s)"));
        assert!(!fs.called("read_dir(/d)"));
        assert_eq!(s.unreadable, 1);
        assert!(!fs.called("open_read("));
    }

    /// A run that must stop at the root record: the one row, and no directory listed or file opened.
    fn root_row(fs: &FaultFs, src: &str, dst: &str) -> Row {
        let (rows, s) = run(fs, src, dst, &opts());
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(s.unreadable, 1);
        assert!(!fs.called("read_dir(") && !fs.called("open_read("));
        assert_eq!(
            (rows[0].0.as_str(), rows[0].1, rows[0].2),
            (".", Outcome::Unreadable, Kind::Dir)
        );
        rows[0].clone()
    }

    #[test]
    fn a_root_check_failure_names_the_root_it_was_about() {
        // The destination is a file: `destination_root(dst)` fails with a kind other than NotFound.
        let fs = FaultFs::new();
        tree(&fs, "/s", &[], &[]);
        fs.write_file("/d", b"x");
        let r = root_row(&fs, "/s", "/d");
        assert!(r.4.starts_with("destination: "), "{}", r.4);

        // The source is a file.
        let fs = FaultFs::new();
        tree(&fs, "/d", &[], &[]);
        fs.write_file("/s", b"x");
        let r = root_row(&fs, "/s", "/d");
        assert!(r.4.starts_with("source: "), "{}", r.4);

        // canonical_path calls: containment(dst anchor): /d (1), /s (2); containment(src anchor): /s (3), /d (4). Calls 1 and 3 are the anchors.
        let denied = std::io::ErrorKind::PermissionDenied;
        let fs = FaultFs::new();
        tree(&fs, "/s", &[], &[]);
        tree(&fs, "/d", &[], &[]);
        fs.fail_nth("canonical_path", 2, Code::PermissionDenied, denied);
        let r = root_row(&fs, "/s", "/d");
        assert_eq!(r.3, Some(Code::PermissionDenied));
        assert!(r.4.starts_with("source: "), "{}", r.4);

        let fs = FaultFs::new();
        tree(&fs, "/s", &[], &[]);
        tree(&fs, "/d", &[], &[]);
        fs.fail_nth("canonical_path", 4, Code::PermissionDenied, denied);
        let r = root_row(&fs, "/s", "/d");
        assert!(r.4.starts_with("destination: "), "{}", r.4);

        let fs = FaultFs::new();
        tree(&fs, "/s", &[], &[]);
        tree(&fs, "/d", &[], &[]);
        fs.fail_nth("canonical_path", 1, Code::PermissionDenied, denied);
        let r = root_row(&fs, "/s", "/d");
        assert_eq!(r.3, Some(Code::PermissionDenied));
        assert!(r.4.starts_with("destination: "), "{}", r.4);

        let fs = FaultFs::new();
        tree(&fs, "/s", &[], &[]);
        tree(&fs, "/d", &[], &[]);
        fs.fail_nth("canonical_path", 3, Code::PermissionDenied, denied);
        let r = root_row(&fs, "/s", "/d");
        assert_eq!(r.3, Some(Code::PermissionDenied));
        assert!(r.4.starts_with("source: "), "{}", r.4);
    }

    #[test]
    fn the_absent_destination_notice_fires_once_after_the_root_check_and_not_on_a_refusal() {
        let fs = FaultFs::new();
        tree(&fs, "/s", &[], &[("a", b"x")]);
        let mut events = Vec::new();
        let events_ref = std::cell::RefCell::new(&mut events);
        let r = verify_tree_notifying(
            &fs,
            Path::new("/s"),
            Path::new("/nope"),
            &opts(),
            &mut || events_ref.borrow_mut().push("absent".to_string()),
            &mut |r| events_ref.borrow_mut().push(format!("record {}", r.path.display())),
        );
        assert!(r.is_ok());
        assert_eq!(events, ["absent", "record a"]);

        let mut called = false;
        let r = verify_tree_notifying(
            &fs,
            Path::new("/s"),
            Path::new("/s/nope"),
            &opts(),
            &mut || called = true,
            &mut |_| {},
        );
        assert!(matches!(r, Err(VerifyError::RefusedRoots { .. })));
        assert!(!called);

        tree(&fs, "/d", &[], &[("a", b"x")]);
        let mut called = false;
        verify_tree_notifying(
            &fs,
            Path::new("/s"),
            Path::new("/d"),
            &opts(),
            &mut || called = true,
            &mut |_| {},
        )
        .unwrap();
        assert!(!called, "an existing destination is not announced");
    }

    #[test]
    fn a_mount_point_on_either_side_is_matched_once_and_pruned() {
        let o = VerifyOptions { mounts: MountPolicy::Stay, ..opts() };
        let want =
            vec![("m".to_string(), Outcome::Match, Kind::Dir, None, MOUNT_NOT_ENTERED.to_string())];

        let fs = FaultFs::new();
        tree(&fs, "/s", &["m"], &[("m/x", b"x")]);
        tree(&fs, "/d", &["m"], &[]);
        fs.set_mount_root("/d/m", MountRoot::Yes);
        let (rows, s) = run(&fs, "/s", "/d", &o);
        assert_eq!(rows, want);
        assert_eq!((s.matched, s.not_entered, s.missing), (1, 1, 0));

        let fs = FaultFs::new();
        tree(&fs, "/s", &["m"], &[]);
        tree(&fs, "/d", &["m"], &[("m/x", b"x")]);
        fs.set_mount_root("/s/m", MountRoot::Yes);
        let (rows, s) = run(&fs, "/s", "/d", &o);
        assert_eq!(rows, want);
        assert_eq!((s.matched, s.not_entered, s.extra), (1, 1, 0));
    }

    #[test]
    fn a_mount_point_present_on_one_side_only_is_not_a_match() {
        let o = VerifyOptions { mounts: MountPolicy::Stay, ..opts() };
        let fs = FaultFs::new();
        tree(&fs, "/s", &["m"], &[]);
        tree(&fs, "/d", &[], &[]);
        fs.set_mount_root("/s/m", MountRoot::Yes);
        let (rows, s) = run(&fs, "/s", "/d", &o);
        assert_eq!(rows, vec![rec("m", Outcome::Missing, Kind::Dir)]);
        assert_eq!(s.not_entered, 0);
    }

    #[test]
    fn a_source_file_against_a_destination_mount_point_is_a_mismatch_and_nothing_below_it_is_seen()
    {
        let o = VerifyOptions { mounts: MountPolicy::Stay, ..opts() };
        let fs = FaultFs::new();
        tree(&fs, "/s", &[], &[("m", b"m")]);
        tree(&fs, "/d", &["m"], &[("m/x", b"x")]);
        fs.set_mount_root("/d/m", MountRoot::Yes);
        let (rows, s) = run(&fs, "/s", "/d", &o);
        assert_eq!(rows, vec![mm("m", Kind::File, "source file, destination dir")]);
        assert_eq!((s.mismatched, s.not_entered, s.extra), (1, 0, 0));
        assert!(!fs.called("read_dir(/d/m)"));
    }

    #[test]
    fn a_pruned_directory_does_not_hide_a_sibling_whose_name_starts_with_the_same_bytes() {
        let fs = FaultFs::new();
        tree(&fs, "/s", &["a", "z"], &[("z/a0", &[2u8; 17])]);
        fs.set_mount_root("/s/a", MountRoot::Yes);
        fs.fail_read_dir_of("/s/a", Code::PermissionDenied, std::io::ErrorKind::PermissionDenied);
        fs.add_symlink_to("/s/a-b", "t", LinkKind::Unspecified);
        tree(&fs, "/d", &["z"], &[("z/a0", &[2u8; 17])]);
        fs.add_symlink_to("/d/a-b", "t", LinkKind::Unspecified);
        let (rows, s) = run(&fs, "/s", "/d", &opts());
        assert_eq!(
            rows,
            vec![
                (
                    "a".to_string(),
                    Outcome::Unreadable,
                    Kind::Dir,
                    Some(Code::PermissionDenied),
                    "permission denied".to_string()
                ),
                rec("a-b", Outcome::Match, Kind::Symlink),
                rec("z", Outcome::Match, Kind::Dir),
                rec("z/a0", Outcome::Match, Kind::File),
            ]
        );
        assert_eq!((s.matched, s.unreadable, s.bytes_hashed), (3, 1, 34));
    }

    #[test]
    fn two_mount_points_not_entered_in_one_run_each_keep_their_own_prune() {
        let o = VerifyOptions { mounts: MountPolicy::Stay, ..opts() };
        let fs = FaultFs::new();
        tree(&fs, "/s", &["a-b", "a0"], &[("a0/a-b", b"")]);
        fs.set_mount_root("/s/a-b", MountRoot::Yes);
        fs.set_mount_root("/s/a0", MountRoot::Yes);
        tree(&fs, "/d", &["a-b", "a0"], &[("a-b/x", b""), ("a0/a-b", b"")]);
        fs.set_mount_root("/d/a-b", MountRoot::Unknown);
        let (rows, s) = run(&fs, "/s", "/d", &o);
        let not_entered = |p: &str| {
            (p.to_string(), Outcome::Match, Kind::Dir, None, MOUNT_NOT_ENTERED.to_string())
        };
        assert_eq!(rows, vec![not_entered("a-b"), not_entered("a0")]);
        assert_eq!((s.matched, s.not_entered), (2, 2));
    }

    #[test]
    fn a_directory_faulted_on_both_sides_reports_the_source_code() {
        let fs = FaultFs::new();
        tree(&fs, "/s", &["b", "b/a-b", "b/a.txt"], &[]);
        fs.set_mount_root("/s/b/a-b", MountRoot::Yes);
        fs.fail_read_dir_of("/s/b/a-b", Code::IoError, std::io::ErrorKind::Other);
        tree(&fs, "/d", &["b", "b/a-b", "b/a.txt"], &[]);
        fs.fail_read_dir_of(
            "/d/b/a-b",
            Code::PermissionDenied,
            std::io::ErrorKind::PermissionDenied,
        );
        let (rows, s) = run(&fs, "/s", "/d", &opts());
        assert_eq!(
            rows,
            vec![
                rec("b", Outcome::Match, Kind::Dir),
                (
                    "b/a-b".to_string(),
                    Outcome::Unreadable,
                    Kind::Dir,
                    Some(Code::IoError),
                    "other error".to_string()
                ),
                rec("b/a.txt", Outcome::Match, Kind::Dir),
            ]
        );
        assert_eq!((s.matched, s.unreadable), (2, 1));
    }

    #[test]
    fn cross_filesystems_enters_mount_points() {
        let fs = FaultFs::new();
        tree(&fs, "/s", &["m"], &[("m/x", b"x")]);
        tree(&fs, "/d", &["m"], &[("m/x", b"x")]);
        fs.set_mount_root("/d/m", MountRoot::Yes);
        let (rows, s) = run(&fs, "/s", "/d", &opts());
        assert_eq!(
            rows,
            vec![rec("m", Outcome::Match, Kind::Dir), rec("m/x", Outcome::Match, Kind::File)]
        );
        assert_eq!((s.matched, s.not_entered), (2, 0));
    }

    fn run_file(fs: &FaultFs, src: &str, dst: &str) -> (Vec<Row>, Summary) {
        let mut rows = Vec::new();
        let s = verify_file(fs, Path::new(src), Path::new(dst), &opts(), &mut |r| {
            rows.push(row(&r));
        })
        .unwrap();
        (rows, s)
    }

    #[test]
    fn verify_file_matches_mismatches_and_misses() {
        let fs = FaultFs::new();
        tree(&fs, "/s", &[], &[("a", b"alpha")]);
        tree(&fs, "/d", &[], &[("a", b"alpha")]);
        let before = fs.calls().len();
        let (rows, s) = run_file(&fs, "/s/a", "/d/a");
        assert_eq!(rows, vec![rec("a", Outcome::Match, Kind::File)]);
        assert_eq!(s, Summary { matched: 1, bytes_hashed: 10, ..Summary::default() });
        assert_no_writes(&fs, before);

        let fs = FaultFs::new();
        tree(&fs, "/s", &[], &[("a", b"alpha")]);
        tree(&fs, "/d", &[], &[("a", b"bravo")]);
        let before = fs.calls().len();
        let (rows, s) = run_file(&fs, "/s/a", "/d/a");
        assert_eq!(rows.len(), 1);
        let (path, outcome, kind, code, detail) = &rows[0];
        assert_eq!(
            (path.as_str(), *outcome, *kind, *code),
            ("a", Outcome::Mismatched, Kind::File, Some(Code::VerifyMismatch))
        );
        let parts: Vec<&str> = detail.split(" != ").collect();
        assert_eq!(parts.len(), 2, "{detail}");
        assert!(parts.iter().all(|p| p.len() == 8 && p.chars().all(|c| c.is_ascii_hexdigit())));
        assert_ne!(parts[0], parts[1]);
        assert_eq!((s.mismatched, s.bytes_hashed), (1, 10));
        assert!(s.failed());
        assert_no_writes(&fs, before);

        let fs = FaultFs::new();
        tree(&fs, "/s", &[], &[("a", b"alpha")]);
        tree(&fs, "/d", &[], &[]);
        let before = fs.calls().len();
        let (rows, s) = run_file(&fs, "/s/a", "/d/a");
        assert_eq!(rows, vec![rec("a", Outcome::Missing, Kind::File)]);
        assert_eq!((s.missing, s.bytes_hashed), (1, 0));
        assert!(!fs.called("open_read("));
        assert_no_writes(&fs, before);
    }

    #[test]
    fn verify_file_against_a_directory_or_a_special_is_a_mismatch_or_unreadable() {
        let fs = FaultFs::new();
        tree(&fs, "/s", &[], &[("a", b"alpha")]);
        tree(&fs, "/d", &["a"], &[]);
        let (rows, s) = run_file(&fs, "/s/a", "/d/a");
        assert_eq!(
            rows,
            vec![(
                "a".to_string(),
                Outcome::Mismatched,
                Kind::File,
                Some(Code::VerifyMismatch),
                "source file, destination dir".to_string()
            )]
        );
        assert_eq!(s.mismatched, 1);
        assert!(!fs.called("open_read("));

        let fs = FaultFs::new();
        tree(&fs, "/s", &[], &[]);
        tree(&fs, "/d", &[], &[]);
        fs.add_special("/s/a");
        fs.add_special("/d/a");
        let (rows, s) = run_file(&fs, "/s/a", "/d/a");
        assert_eq!(
            rows,
            vec![(
                "a".to_string(),
                Outcome::Unreadable,
                Kind::Other,
                Some(Code::SpecialFileUnsupported),
                SPECIAL_NOT_COMPARED.to_string()
            )]
        );
        assert_eq!(s.unreadable, 1);
        assert!(!fs.called("open_read("));
    }

    #[test]
    fn verify_file_reports_a_source_read_error_without_opening_the_destination() {
        let fs = FaultFs::new();
        tree(&fs, "/s", &[], &[("a", b"alpha")]);
        tree(&fs, "/d", &[], &[("a", b"alpha")]);
        fs.fail_read_nth("/s/a", 1, std::io::ErrorKind::Other);
        let (rows, s) = run_file(&fs, "/s/a", "/d/a");
        let classified = FsError::classify(&std::io::Error::from(std::io::ErrorKind::Other));
        assert_eq!(rows.len(), 1);
        assert_eq!(
            (rows[0].0.as_str(), rows[0].1, rows[0].2, rows[0].3),
            ("a", Outcome::Unreadable, Kind::File, Some(classified))
        );
        assert!(!rows[0].4.is_empty());
        assert!(!fs.called("open_read(/d/a)"));
        assert_eq!((s.unreadable, s.bytes_hashed), (1, 0));
    }

    #[test]
    fn verify_file_reports_a_destination_metadata_error_as_unreadable_not_missing() {
        let fs = FaultFs::new();
        tree(&fs, "/s", &[], &[("a", b"alpha")]);
        tree(&fs, "/d", &[], &[("a", b"alpha")]);
        // verify_file calls metadata(src) then metadata(dst), so call 2 is metadata(/d/a).
        fs.fail_nth("metadata", 2, Code::PermissionDenied, std::io::ErrorKind::PermissionDenied);
        let (rows, s) = run_file(&fs, "/s/a", "/d/a");
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(
            (rows[0].0.as_str(), rows[0].1, rows[0].2, rows[0].3),
            ("a", Outcome::Unreadable, Kind::File, Some(Code::PermissionDenied))
        );
        assert!(!rows[0].4.is_empty());
        assert!(fs.called("metadata(/d/a)"));
        assert!(!fs.called("open_read("));
        assert_eq!((s.unreadable, s.missing, s.bytes_hashed), (1, 0, 0));
    }

    #[test]
    fn check_pair_classifies_a_checked_an_absent_and_a_refused_pair() {
        let fs = FaultFs::new();
        tree(&fs, "/s", &[], &[]);
        tree(&fs, "/d", &[], &[]);
        assert!(matches!(
            check_pair(&fs, Path::new("/s"), Path::new("/d")),
            Ok(RootVerdict::Checked { .. })
        ));
        assert!(matches!(
            check_pair(&fs, Path::new("/s"), Path::new("/gone")),
            Ok(RootVerdict::Absent)
        ));
        tree(&fs, "/s/d", &[], &[]);
        let reason = check_pair(&fs, Path::new("/s"), Path::new("/s/d")).unwrap_err();
        assert!(reason.starts_with(REFUSED_DST_IN_SRC), "{reason}");
        match check_pair(&fs, Path::new("/nosrc"), Path::new("/d")) {
            Ok(RootVerdict::Unreadable { side, .. }) => assert_eq!(side, SOURCE),
            _ => panic!("an absent source must be unreadable"),
        }
    }

    fn spec(src: &str, name: &str, kind: crate::run::SourceKind) -> crate::run::SourceSpec {
        crate::run::SourceSpec { src: PathBuf::from(src), name: name.into(), kind }
    }

    type Several = (Vec<Row>, Vec<(String, Summary)>, Vec<String>, Summary);
    type SeveralRaw = (Vec<Row>, Vec<(String, Summary)>, Vec<String>, Result<Summary, VerifyError>);

    fn several_result(
        fs: &FaultFs,
        specs: Vec<crate::run::SourceSpec>,
        dest: &str,
        o: &VerifyOptions,
    ) -> SeveralRaw {
        let mut rows = Vec::new();
        let mut roots = Vec::new();
        let mut absent = Vec::new();
        let total = verify_several(
            fs,
            specs,
            Path::new(dest),
            o,
            &mut |n| absent.push(n.to_string_lossy().into_owned()),
            &mut |n, s| roots.push((n.to_string_lossy().into_owned(), s.clone())),
            &mut |r| rows.push(row(&r)),
        );
        (rows, roots, absent, total)
    }

    fn several(
        fs: &FaultFs,
        specs: Vec<crate::run::SourceSpec>,
        dest: &str,
        o: &VerifyOptions,
    ) -> Several {
        let (rows, roots, absent, total) = several_result(fs, specs, dest, o);
        (rows, roots, absent, total.unwrap())
    }

    /// `/s/<n>` and `/d/<n>` for each name, empty; the case-sensitive fake models an aliasing destination through
    /// `set_identity`.
    fn alias_fixture(names: &[&str]) -> FaultFs {
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/s")).unwrap();
        fs.create_dir(Path::new("/d")).unwrap();
        for n in names {
            tree(&fs, &format!("/s/{n}"), &[], &[]);
            tree(&fs, &format!("/d/{n}"), &[], &[]);
        }
        fs
    }

    fn one_id() -> flux_fs::ObjectId {
        flux_fs::ObjectId { volume: 1, index: 999 }
    }

    fn alias_pair(fs: &FaultFs, id: flux_fs::FileIdentity) {
        fs.set_identity("/d/Data", id);
        fs.set_identity("/d/data", id);
    }

    fn data_specs() -> Vec<crate::run::SourceSpec> {
        use crate::run::SourceKind::Folder;
        vec![spec("/s/data", "data", Folder), spec("/s/Data", "Data", Folder)]
    }

    #[test]
    fn two_root_names_that_alias_on_the_destination_are_refused_before_any_read() {
        let strong = flux_fs::FileIdentity::Strong(one_id());
        // Arm 1: one Strong identity for two directories.
        let fs = alias_fixture(&["Data", "data"]);
        alias_pair(&fs, strong);
        let (rows, _, _, r) = several_result(&fs, data_specs(), "/d", &opts());
        match r {
            Err(VerifyError::RefusedAlias { a, b }) => {
                assert_eq!((a.to_str(), b.to_str()), (Some("Data"), Some("data")));
            }
            other => panic!("expected RefusedAlias, got {other:?}"),
        }
        assert!(!fs.called("read_dir(/s/Data)") && !fs.called("read_dir(/s/data)"));
        assert!(!fs.called("read_dir(/d/Data)") && !fs.called("read_dir(/d/data)"));
        assert!(rows.is_empty(), "{rows:?}");

        // Arm 2: their own minted identities.
        let fs = alias_fixture(&["Data", "data"]);
        let (_, roots, _, r) = several_result(&fs, data_specs(), "/d", &opts());
        assert!(r.is_ok(), "{r:?}");
        assert_eq!(roots.len(), 2);

        // Arm 3: equal but Weak.
        let fs = alias_fixture(&["Data", "data"]);
        alias_pair(&fs, flux_fs::FileIdentity::Weak(one_id()));
        let (_, roots, _, r) = several_result(&fs, data_specs(), "/d", &opts());
        assert!(r.is_ok(), "{r:?}");
        assert_eq!(roots.len(), 2);

        // Arm 4: equal and Strong, but Data's source is absent: no identity was obtained for it.
        let fs = alias_fixture(&["data"]);
        fs.create_dir(Path::new("/d/Data")).unwrap();
        alias_pair(&fs, strong);
        let (rows, roots, _, r) = several_result(&fs, data_specs(), "/d", &opts());
        assert!(r.is_ok(), "{r:?}");
        assert_eq!(roots.len(), 2);
        assert!(rows.iter().any(|x| x.0 == "Data" && x.1 == Outcome::Unreadable), "{rows:?}");

        // Arm 5: an absent third root beside the aliasing pair does not hide the alias.
        use crate::run::SourceKind::Folder;
        let fs = alias_fixture(&["Data", "data"]);
        alias_pair(&fs, strong);
        let mut specs = data_specs();
        specs.push(spec("/s/other", "other", Folder));
        let (_, _, _, r) = several_result(&fs, specs, "/d", &opts());
        assert!(matches!(r, Err(VerifyError::RefusedAlias { .. })), "{r:?}");
    }

    #[test]
    fn a_refused_root_check_wins_over_an_alias_and_discards_remembered_verdicts() {
        use crate::run::SourceKind::Folder;
        let build = || {
            let fs = alias_fixture(&["Data", "data"]);
            alias_pair(&fs, flux_fs::FileIdentity::Strong(one_id()));
            fs.create_dir(Path::new("/d/a")).unwrap();
            fs
        };
        let base = || {
            let mut specs = data_specs();
            specs.push(spec("/s/a", "a", Folder));
            specs
        };
        // Control: no refused root, so the alias is what comes back (and `a`, unreadable, is discarded).
        let fs = build();
        let (rows, _, _, r) = several_result(&fs, base(), "/d", &opts());
        assert!(matches!(r, Err(VerifyError::RefusedAlias { .. })), "{r:?}");
        assert!(rows.is_empty(), "{rows:?}");

        let fs = build();
        let mut specs = base();
        specs.push(spec("/d", "z", Folder));
        let (rows, _, _, r) = several_result(&fs, specs, "/d", &opts());
        match r {
            Err(VerifyError::RefusedRoot { name, .. }) => assert_eq!(name.to_str(), Some("z")),
            other => panic!("expected RefusedRoot, got {other:?}"),
        }
        assert!(rows.is_empty(), "{rows:?}");
    }

    #[test]
    fn several_roots_stream_in_prefix_order_with_re_rooted_paths() {
        use crate::run::SourceKind::Folder;
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/s")).unwrap();
        tree(&fs, "/d", &["a", "a-b", "b"], &[("a/x", b"1"), ("a-b/x", b"1"), ("b/x", b"1")]);
        for n in ["a", "a-b", "b"] {
            tree(&fs, &format!("/s/{n}"), &[], &[("x", b"1")]);
        }
        let specs = vec![
            spec("/s/b", "b", Folder),
            spec("/s/a-b", "a-b", Folder),
            spec("/s/a", "a", Folder),
        ];
        let (rows, roots, absent, total) = several(&fs, specs, "/d", &opts());
        assert_eq!(
            rows,
            vec![
                rec("a/x", Outcome::Match, Kind::File),
                rec("a-b/x", Outcome::Match, Kind::File),
                rec("b/x", Outcome::Match, Kind::File),
            ]
        );
        let names: Vec<_> = roots.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["a", "a-b", "b"]);
        assert!(absent.is_empty());
        assert_eq!(total.matched, 3);
    }

    #[test]
    fn a_file_root_is_one_record_named_after_the_root() {
        use crate::run::SourceKind::File;
        let fs = FaultFs::new();
        tree(&fs, "/s", &[], &[("f", b"abc")]);
        tree(&fs, "/d", &[], &[("f", b"abc")]);
        let (rows, roots, _, _) = several(&fs, vec![spec("/s/f", "f", File)], "/d", &opts());
        assert_eq!(rows, vec![rec("f", Outcome::Match, Kind::File)]);
        assert_eq!(roots.len(), 1);

        let fs = FaultFs::new();
        tree(&fs, "/s", &[], &[("f", b"abc")]);
        tree(&fs, "/d", &[], &[]);
        let (rows, _, _, total) = several(&fs, vec![spec("/s/f", "f", File)], "/d", &opts());
        assert_eq!(rows, vec![rec("f", Outcome::Missing, Kind::File)]);
        assert_eq!(total.missing, 1);
    }

    #[test]
    fn an_absent_dest_reports_every_root_missing_and_notifies_each_folder_root() {
        use crate::run::SourceKind::{File, Folder};
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/s")).unwrap();
        tree(&fs, "/s/a", &[], &[("x", b"1"), ("y", b"2")]);
        tree(&fs, "/s/b", &[], &[("z", b"3")]);
        fs.write_file("/s/f", b"4");
        let specs =
            vec![spec("/s/f", "f", File), spec("/s/b", "b", Folder), spec("/s/a", "a", Folder)];
        let (rows, _, absent, total) = several(&fs, specs, "/d", &opts());
        assert_eq!(absent, ["a", "b"]);
        assert_eq!(rows.len(), 4);
        assert!(rows.iter().all(|r| r.1 == Outcome::Missing), "{rows:?}");
        assert_eq!(total.missing, 4);
    }

    #[test]
    fn a_root_created_after_it_was_checked_absent_still_reports_missing() {
        use crate::run::SourceKind::Folder;
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/s")).unwrap();
        fs.create_dir(Path::new("/d")).unwrap();
        tree(&fs, "/s/a", &[], &[("x", b"1")]);
        let mut rows = Vec::new();
        let total = verify_several(
            &fs,
            vec![spec("/s/a", "a", Folder)],
            Path::new("/d"),
            &opts(),
            // The destination appears between the check phase and the walk (design 6.2: no re-probe).
            &mut |_| tree(&fs, "/d/a", &[], &[("x", b"1")]),
            &mut |_, _| {},
            &mut |r| rows.push(row(&r)),
        )
        .unwrap();
        assert!(fs.exists(Path::new("/d/a/x")), "the fixture must have created the destination");
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!((rows[0].0.as_str(), rows[0].1), ("a/x", Outcome::Missing));
        assert_eq!(total.missing, 1);
        assert_eq!(total.matched, 0);
    }

    #[test]
    fn a_mount_notice_carries_the_re_rooted_path() {
        use crate::run::SourceKind::Folder;
        let o = VerifyOptions { mounts: MountPolicy::Stay, ..opts() };
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/s")).unwrap();
        fs.create_dir(Path::new("/d")).unwrap();
        tree(&fs, "/s/b", &["m"], &[]);
        tree(&fs, "/d/b", &["m"], &[]);
        fs.set_mount_root("/s/b/m", MountRoot::Yes);
        fs.set_mount_root("/d/b/m", MountRoot::Yes);
        let (rows, _, _, total) = several(&fs, vec![spec("/s/b", "b", Folder)], "/d", &o);
        assert_eq!(
            rows,
            vec![(
                "b/m".to_string(),
                Outcome::Match,
                Kind::Dir,
                None,
                MOUNT_NOT_ENTERED.to_string()
            )]
        );
        assert_eq!(total.not_entered, 1);
    }

    #[test]
    fn per_root_summaries_sum_to_the_total() {
        use crate::run::SourceKind::Folder;
        let o = VerifyOptions { mounts: MountPolicy::Stay, ..opts() };
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/s")).unwrap();
        fs.create_dir(Path::new("/d")).unwrap();
        tree(&fs, "/s/a", &[], &[("p", b"aa")]);
        tree(&fs, "/d/a", &[], &[("p", b"bb"), ("e", b"e")]);
        tree(&fs, "/s/b", &["m"], &[("u", b"u")]);
        tree(&fs, "/d/b", &["m"], &[]);
        fs.set_mount_root("/s/b/m", MountRoot::Yes);
        fs.set_mount_root("/d/b/m", MountRoot::Yes);
        tree(&fs, "/s/c", &[], &[("k", b"kkk"), ("l", b"lll")]);
        tree(&fs, "/d/c", &[], &[("k", b"kkk"), ("l", b"lll")]);
        fs.create_dir(Path::new("/d/d")).unwrap();
        let specs = ["a", "b", "c", "d"].map(|n| spec(&format!("/s/{n}"), n, Folder)).to_vec();
        let (_, roots, _, total) = several(&fs, specs, "/d", &o);
        let names: Vec<_> = roots.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["a", "b", "c", "d"]);
        let mut sum = Summary::default();
        for (_, s) in &roots {
            sum.matched += s.matched;
            sum.missing += s.missing;
            sum.mismatched += s.mismatched;
            sum.extra += s.extra;
            sum.unreadable += s.unreadable;
            sum.not_entered += s.not_entered;
            sum.excluded += s.excluded;
            sum.bytes_hashed += s.bytes_hashed;
        }
        assert_eq!(sum, total);
        for (field, v) in [
            ("matched", total.matched),
            ("missing", total.missing),
            ("mismatched", total.mismatched),
            ("extra", total.extra),
            ("unreadable", total.unreadable),
            ("not_entered", total.not_entered),
            ("bytes_hashed", total.bytes_hashed),
        ] {
            assert!(v > 0, "{field} is zero in {total:?}");
        }
    }

    #[test]
    fn a_source_inside_another_source_is_its_own_root() {
        use crate::run::SourceKind::{File, Folder};
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/s")).unwrap();
        tree(&fs, "/s/a", &[], &[("f", b"1"), ("g", b"2")]);
        tree(&fs, "/d", &["a"], &[("a/f", b"1"), ("a/g", b"2"), ("f", b"1")]);
        let specs = vec![spec("/s/a/f", "f", File), spec("/s/a", "a", Folder)];
        let (rows, _, _, _) = several(&fs, specs, "/d", &opts());
        assert_eq!(
            rows,
            vec![
                rec("a/f", Outcome::Match, Kind::File),
                rec("a/g", Outcome::Match, Kind::File),
                rec("f", Outcome::Match, Kind::File),
            ]
        );
    }

    #[test]
    fn a_users_dot_flux_inside_a_root_is_compared_through_verify_several() {
        use crate::run::SourceKind::Folder;
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/s")).unwrap();
        fs.create_dir(Path::new("/d")).unwrap();
        tree(&fs, "/s/a", &[".flux"], &[(".flux/x", b"x"), (".flux-root.lock", b"l")]);
        let partial = format!("f.flux-partial.{ZEROS}");
        tree(
            &fs,
            "/d/a",
            &[".flux"],
            &[(".flux/x", b"x"), (".flux-root.lock", b"l"), (partial.as_str(), b"p")],
        );
        let (rows, _, _, _) = several(&fs, vec![spec("/s/a", "a", Folder)], "/d", &opts());
        assert_eq!(
            rows,
            vec![
                rec("a/.flux", Outcome::Match, Kind::Dir),
                rec("a/.flux/x", Outcome::Match, Kind::File),
                rec("a/.flux-root.lock", Outcome::Match, Kind::File),
            ]
        );
    }

    #[test]
    fn a_root_check_refusal_on_a_later_root_prints_nothing() {
        use crate::run::SourceKind::Folder;
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/s")).unwrap();
        tree(&fs, "/s/a", &[], &[("x", b"1")]);
        tree(&fs, "/d", &["a", "b"], &[("a/x", b"1")]);
        let specs = vec![spec("/s/a", "a", Folder), spec("/d", "b", Folder)];
        let (rows, roots, absent, total) = several_result(&fs, specs, "/d", &opts());
        let Err(VerifyError::RefusedRoot { name, reason }) = total else {
            panic!("expected RefusedRoot");
        };
        assert_eq!(name, std::ffi::OsString::from("b"));
        assert_eq!(reason, "the destination is the source or lies inside it");
        assert!(rows.is_empty() && roots.is_empty() && absent.is_empty());
    }

    #[test]
    fn two_sources_with_one_name_are_refused_before_any_read() {
        use crate::run::SourceKind::Folder;
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/s")).unwrap();
        tree(&fs, "/s/p", &[], &[]);
        tree(&fs, "/s/q", &[], &[]);
        tree(&fs, "/d", &[], &[]);
        let specs = vec![spec("/s/p", "x", Folder), spec("/s/q", "x", Folder)];
        let (rows, _, _, total) = several_result(&fs, specs, "/d", &opts());
        assert!(matches!(total, Err(VerifyError::RefusedNames { .. })));
        assert!(rows.is_empty());
        let calls = fs.calls();
        assert!(!calls.iter().any(|c| c.starts_with("read_dir(") || c.starts_with("open_read(")));
    }
    #[test]
    fn a_root_whose_destination_has_the_wrong_type_is_one_mismatched_record_and_the_run_goes_on() {
        use crate::run::SourceKind::{File, Folder};
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/s")).unwrap();
        for n in ["a", "b", "c"] {
            tree(&fs, &format!("/s/{n}"), &[], &[("x", b"1")]);
        }
        fs.write_file("/s/f", b"1");
        tree(&fs, "/d", &["b", "f"], &[("b/x", b"1")]);
        fs.write_file("/d/a", b"not a dir");
        fs.add_symlink_to("/d/c", "/d/b", LinkKind::Dir);
        let specs = vec![
            spec("/s/c", "c", Folder),
            spec("/s/b", "b", Folder),
            spec("/s/a", "a", Folder),
            spec("/s/f", "f", File),
        ];
        let (rows, roots, _, total) = several(&fs, specs, "/d", &opts());
        assert_eq!(
            rows,
            vec![
                mm("a", Kind::Dir, "source dir, destination file"),
                rec("b/x", Outcome::Match, Kind::File),
                mm("c", Kind::Dir, "source dir, destination symlink"),
                mm("f", Kind::File, "source file, destination dir"),
            ]
        );
        assert!(!fs.called("read_dir(/d/a)"));
        assert!(!fs.called("read_dir(/d/c)"));
        let names: Vec<_> = roots.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["a", "b", "c", "f"]);
        let one = Summary { mismatched: 1, ..Summary::default() };
        assert_eq!(roots[0].1, one);
        assert_eq!(roots[2].1, one);
        assert_eq!(roots[3].1, one);
        let mut sum = Summary::default();
        for (_, r) in &roots {
            sum.matched += r.matched;
            sum.missing += r.missing;
            sum.mismatched += r.mismatched;
            sum.extra += r.extra;
            sum.unreadable += r.unreadable;
            sum.not_entered += r.not_entered;
            sum.excluded += r.excluded;
            sum.bytes_hashed += r.bytes_hashed;
        }
        assert_eq!(total, sum);
        assert_eq!(total.mismatched, 3);
    }

    #[test]
    fn an_unreadable_root_is_one_record_at_its_turn_and_the_others_run() {
        use crate::run::SourceKind::Folder;
        let build = || {
            let fs = FaultFs::new();
            fs.create_dir(Path::new("/s")).unwrap();
            tree(&fs, "/s/b", &[], &[("x", b"1")]);
            tree(&fs, "/d", &["a", "b"], &[("b/x", b"1")]);
            fs
        };
        let specs = || vec![spec("/s/a", "a", Folder), spec("/s/b", "b", Folder)];
        let check = |fs: &FaultFs, lead: &str, unreadable_code: Option<Code>| {
            let (rows, roots, _, total) = several(fs, specs(), "/d", &opts());
            assert_eq!(rows.len(), 2, "{rows:?}");
            assert_eq!(rows[0].0, "a");
            assert_eq!(rows[0].1, Outcome::Unreadable);
            if unreadable_code.is_some() {
                assert_eq!(rows[0].3, unreadable_code);
            }
            assert!(rows[0].4.starts_with(lead), "{}", rows[0].4);
            assert_eq!(rows[1], rec("b/x", Outcome::Match, Kind::File));
            let names: Vec<_> = roots.iter().map(|(n, _)| n.as_str()).collect();
            assert_eq!(names, ["a", "b"]);
            assert_eq!(roots[0].1.unreadable, 1);
            assert_eq!(roots[1].1.matched, 1);
            assert_eq!(total.unreadable, 1);
        };

        // (i) check phase: the source root is absent.
        let fs = build();
        check(&fs, SOURCE, None);

        // (ii) probe phase: the first metadata(/d/a) of the run.
        let fs2 = build();
        tree(&fs2, "/s/a", &[], &[("x", b"1")]);
        fs2.write_file("/d/a/x", b"1");
        let _ = several(&fs2, specs(), "/d", &opts());
        let n = fs2
            .calls()
            .iter()
            .filter(|c| c.starts_with("metadata("))
            .position(|c| c == "metadata(/d/a)")
            .expect("the unfaulted run probes /d/a")
            + 1;
        let fs3 = build();
        tree(&fs3, "/s/a", &[], &[("x", b"1")]);
        fs3.write_file("/d/a/x", b"1");
        fs3.fail_nth(
            "metadata",
            u32::try_from(n).unwrap(),
            Code::PermissionDenied,
            std::io::ErrorKind::PermissionDenied,
        );
        check(&fs3, DESTINATION, Some(Code::PermissionDenied));

        // (iii) walk phase: Side::new lists the source root.
        let fs4 = build();
        tree(&fs4, "/s/a", &[], &[("x", b"1")]);
        fs4.write_file("/d/a/x", b"1");
        fs4.fail_read_dir_of("/s/a", Code::PermissionDenied, std::io::ErrorKind::PermissionDenied);
        check(&fs4, SOURCE, Some(Code::PermissionDenied));
    }

    fn selecting(patterns: &[&str], recursive: bool) -> VerifyOptions {
        let patterns: Vec<String> = patterns.iter().map(|p| (*p).to_string()).collect();
        VerifyOptions {
            selection: flux_fs::Selection::compile(&patterns, recursive).unwrap(),
            ..opts()
        }
    }

    #[test]
    fn an_excluded_name_at_the_destination_is_not_extra() {
        let fs = FaultFs::new();
        tree(&fs, "/s", &[], &[("a", b"1")]);
        tree(&fs, "/d", &[], &[("a", b"1"), ("skip", b"2")]);
        let (rows, s) = run(&fs, "/s", "/d", &selecting(&["skip"], true));
        assert_eq!(rows, vec![rec("a", Outcome::Match, Kind::File)]);
        assert_eq!((s.extra, s.excluded), (0, 0));
    }

    #[test]
    fn an_excluded_name_at_the_source_is_not_missing() {
        let fs = FaultFs::new();
        tree(&fs, "/s", &[], &[("a", b"1"), ("skip", b"2")]);
        tree(&fs, "/d", &[], &[("a", b"1")]);
        let (rows, s) = run(&fs, "/s", "/d", &selecting(&["skip"], true));
        assert_eq!(rows, vec![rec("a", Outcome::Match, Kind::File)]);
        assert_eq!((s.missing, s.excluded), (0, 1));
        assert!(!s.failed());
    }

    #[test]
    fn no_recursive_hides_destination_subdirectories_and_exits_clean() {
        let fs = FaultFs::new();
        tree(&fs, "/s", &[], &[("a", b"1")]);
        tree(&fs, "/d", &["extra"], &[("a", b"1"), ("extra/z", b"9")]);
        let (rows, s) = run(&fs, "/s", "/d", &selecting(&[], false));
        assert_eq!(rows, vec![rec("a", Outcome::Match, Kind::File)]);
        assert!(!s.failed());
        assert_eq!((s.extra, s.excluded), (0, 0));
    }

    #[test]
    fn a_top_level_mismatch_still_fails_under_no_recursive() {
        let fs = FaultFs::new();
        tree(&fs, "/s", &["sub"], &[("a", b"1"), ("sub/q", b"1")]);
        tree(&fs, "/d", &["sub"], &[("a", b"2"), ("sub/q", b"1")]);
        let (rows, s) = run(&fs, "/s", "/d", &selecting(&[], false));
        assert_eq!(rows.len(), 1);
        assert_eq!((rows[0].0.as_str(), rows[0].1), ("a", Outcome::Mismatched));
        assert!(s.failed());
    }

    #[test]
    fn a_source_file_against_a_destination_directory_under_a_dir_only_pattern_is_missing() {
        let fs = FaultFs::new();
        tree(&fs, "/s", &[], &[("foo", b"1")]);
        tree(&fs, "/d", &["foo"], &[]);
        let (rows, s) = run(&fs, "/s", "/d", &selecting(&["foo/"], true));
        assert_eq!(rows, vec![rec("foo", Outcome::Missing, Kind::File)]);
        assert_eq!((s.missing, s.excluded), (1, 0));
    }

    #[test]
    fn a_source_directory_against_a_destination_file_under_a_dir_only_pattern_is_extra() {
        let fs = FaultFs::new();
        tree(&fs, "/s", &["foo"], &[]);
        tree(&fs, "/d", &[], &[("foo", b"1")]);
        let (rows, s) = run(&fs, "/s", "/d", &selecting(&["foo/"], true));
        assert_eq!(rows, vec![rec("foo", Outcome::Extra, Kind::File)]);
        assert_eq!((s.extra, s.excluded), (1, 1));
    }

    #[test]
    fn the_count_is_source_side_only_with_asymmetric_sides() {
        let fs = FaultFs::new();
        tree(&fs, "/s", &[], &[("a", b"1"), ("skip", b"2")]);
        tree(&fs, "/d", &[], &[("a", b"1"), ("skip", b"2"), ("skip2", b"3")]);
        let (_, s) = run(&fs, "/s", "/d", &selecting(&["skip", "skip2"], true));
        assert_eq!(s.excluded, 1);
    }

    #[test]
    fn two_roots_sum_their_counts() {
        use crate::run::SourceKind::Folder;
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/s")).unwrap();
        fs.create_dir(Path::new("/d")).unwrap();
        tree(&fs, "/s/a", &[], &[("k", b"1"), ("skip", b"2")]);
        tree(&fs, "/d/a", &[], &[("k", b"1")]);
        tree(&fs, "/s/b", &[], &[("k", b"1"), ("skip", b"2"), ("skip.d", b"3")]);
        tree(&fs, "/d/b", &[], &[("k", b"1")]);
        let o = selecting(&["skip", "skip.d"], true);
        let specs = ["a", "b"].map(|n| spec(&format!("/s/{n}"), n, Folder)).to_vec();
        let (_, roots, _, total) = several(&fs, specs, "/d", &o);
        assert_eq!(roots[0].1.excluded, 1);
        assert_eq!(roots[1].1.excluded, 2);
        assert_eq!(total.excluded, 3);
    }

    #[test]
    fn an_absent_destination_still_reports_the_source_count() {
        let fs = FaultFs::new();
        tree(&fs, "/s", &[], &[("a", b"1"), ("skip", b"2")]);
        let (rows, s) = run(&fs, "/s", "/d", &selecting(&["skip"], true));
        assert_eq!(rows, vec![rec("a", Outcome::Missing, Kind::File)]);
        assert_eq!(s.excluded, 1);
    }

    #[test]
    fn exclusions_under_a_pruned_mount_subtree_still_count() {
        let fs = FaultFs::new();
        tree(&fs, "/s", &["m"], &[("m/x", b"x"), ("m/skip", b"y")]);
        tree(&fs, "/d", &["m"], &[]);
        fs.set_mount_root("/d/m", MountRoot::Yes);
        let o = VerifyOptions { mounts: MountPolicy::Stay, ..selecting(&["skip"], true) };
        let (rows, s) = run(&fs, "/s", "/d", &o);
        assert_eq!(
            rows,
            vec![("m".to_string(), Outcome::Match, Kind::Dir, None, MOUNT_NOT_ENTERED.to_string())]
        );
        assert_eq!((s.not_entered, s.excluded), (1, 1));
    }
}
