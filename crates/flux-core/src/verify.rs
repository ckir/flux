// Copyright (c) 2026 Costas Kirgoussios
//
// This software is licensed under the PolyForm Noncommercial License 1.0.0.
// You may obtain a copy of the License at:
// https://polyformproject.org/licenses/noncommercial/1.0.0/

//! `flux verify`: compare two trees without writing anything (design 6.2).
//!
//! Two walks, one per root, are merged in lockstep by `FluxPathKey` bytes. Each record is handed to the sink as soon
//! as it is decided; the engine holds no list of records.

use crate::walk::{DEFAULT_MAX_DEPTH, MountPolicy, Walk, WalkEvent, WalkOptions, walk_with};
use flux_fs::{Code, DestinationRoot, FileSystem, FluxPathKey, FsError, HashAlgorithm, Links};
use std::cmp::Ordering;
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
    /// `Some(Code::VerifyMismatch)` for every `mismatched`; the error's code for `unreadable`; `None` otherwise.
    pub code: Option<Code>,
    /// Error text for `unreadable`; `"source {a}, destination {b}"` for a type mismatch; `"{s} != {d}"` (first 8 hex
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
#[derive(Debug, Clone, Copy)]
pub struct VerifyOptions {
    pub links: Links,
    pub mounts: MountPolicy,
    pub hash: HashAlgorithm,
}

/// Why a verify did not run.
#[derive(Debug)]
pub enum VerifyError {
    /// Exit 2: the two roots are the same object, or one lies inside the other; nothing was read (design 6.1).
    RefusedRoots { reason: String },
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

/// One root's walk, reduced to `Item`s, with one held item for the merge to look at.
struct Side<'a, F: FileSystem> {
    walk: Option<Walk<'a, F>>,
    /// The failure to open the walk at all: yielded once as an item for the root, then the side is done.
    open_failure: Option<FsError>,
    /// One event of lookahead (a `Dir` is followed by its `MountSkipped`).
    peeked: Option<Option<RawEvent>>,
    held: Option<Item>,
    done: bool,
    pruned: Option<PathBuf>,
    /// The SOURCE side under `Links::Skip`: symlink items are dropped (design 6.3).
    skip_links: bool,
    /// The DEST side: Flux's own control state is dropped (design 6.4).
    control_state: bool,
}

impl<'a, F: FileSystem> Side<'a, F> {
    fn new(
        fs: &'a F,
        root: &Path,
        mounts: MountPolicy,
        skip_links: bool,
        control_state: bool,
    ) -> Self {
        let opened = walk_with(fs, root, WalkOptions { max_depth: DEFAULT_MAX_DEPTH, mounts });
        let (walk, open_failure) = match opened {
            Ok(w) => (Some(w), None),
            Err(e) => (None, Some(e)),
        };
        Side {
            walk,
            open_failure,
            peeked: None,
            held: None,
            done: false,
            pruned: None,
            skip_links,
            control_state,
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

    /// Whether `item` is Flux's own control state (design 6.4): a top-level `.flux` directory with all it holds, the
    /// root-level `.flux-root.lock`, and a copy's `<name>.flux-partial.<id>` temporary at any depth.
    fn is_control_state(item: &Item) -> bool {
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
        if let Some(e) = self.open_failure.take() {
            self.done = true;
            return Some(Self::item(PathBuf::new(), Kind::Dir, false, Some(e)));
        }
        loop {
            let Some(ev) = self.raw() else {
                self.done = true;
                return None;
            };
            let item = match ev {
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
            if self.under_pruned(&item.path)
                || (self.control_state && Self::is_control_state(&item))
            {
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
    let mut a = Side::new(fs, src, opts.mounts, opts.links == Links::Skip, false);
    let mut b = Side::new(fs, dst, opts.mounts, false, true);
    let mut buf = vec![0u8; crate::copy::COPY_BUF_MAX];
    let mut out = Emitter { summary: Summary::default(), sink };
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
                alone(&mut out, &x, Outcome::Missing, &mut a, &mut b);
            }
            Ordering::Greater => {
                let y = b.take().expect("peeked");
                alone(&mut out, &y, Outcome::Extra, &mut a, &mut b);
            }
            Ordering::Equal => {
                let x = a.take().expect("peeked");
                let y = b.take().expect("peeked");
                pair(fs, (src, dst), &mut out, &mut buf, (&x, &y), (&mut a, &mut b));
            }
        }
    }
    Ok(out.summary)
}

/// An item present on one side only: `missing` or `extra`, or `unreadable` (pruning both sides) when its walk failed.
fn alone<F: FileSystem>(
    out: &mut Emitter<'_>,
    item: &Item,
    outcome: Outcome,
    a: &mut Side<'_, F>,
    b: &mut Side<'_, F>,
) {
    match &item.failed {
        Some(e) => {
            out.unreadable(item.path.clone(), item.kind, e.code, e.source.to_string());
            a.prune(&item.path);
            b.prune(&item.path);
        }
        None => out.alone(item, outcome),
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
        (Kind::File, Kind::File) => {
            let (src, dst) = (roots.0.join(&x.path), roots.1.join(&x.path));
            let mut hashed = 0;
            let verdict = compare_files(fs, &src, &dst, buf, &mut hashed);
            out.summary.bytes_hashed += hashed;
            match verdict {
                Ok(None) => out.alone(x, Outcome::Match),
                Ok(Some(detail)) => out.emit(Record {
                    path: x.path.clone(),
                    outcome: Outcome::Mismatched,
                    kind: x.kind,
                    code: Some(Code::VerifyMismatch),
                    detail,
                }),
                Err((code, detail)) => out.unreadable(x.path.clone(), x.kind, code, detail),
            }
        }
        (Kind::Symlink, Kind::Symlink) => {
            let (src, dst) = (roots.0.join(&x.path), roots.1.join(&x.path));
            let payloads = fs.read_link(&src).and_then(|s| Ok((s, fs.read_link(&dst)?)));
            match payloads {
                Ok((s, d)) if s.payload.as_encoded_bytes() == d.payload.as_encoded_bytes() => {
                    out.alone(x, Outcome::Match);
                }
                Ok(_) => out.emit(Record {
                    path: x.path.clone(),
                    outcome: Outcome::Mismatched,
                    kind: Kind::Symlink,
                    code: Some(Code::VerifyMismatch),
                    detail: "symlink payloads differ".to_string(),
                }),
                Err(e) => out.unreadable(x.path.clone(), x.kind, e.code, e.source.to_string()),
            }
        }
        (Kind::Other, Kind::Other) => out.unreadable(
            x.path.clone(),
            Kind::Other,
            Code::SpecialFileUnsupported,
            SPECIAL_NOT_COMPARED.to_string(),
        ),
        (k1, k2) => out.emit(Record {
            path: x.path.clone(),
            outcome: Outcome::Mismatched,
            kind: k1,
            code: Some(Code::VerifyMismatch),
            detail: format!("source {}, destination {}", k1.as_str(), k2.as_str()),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::copy::COPY_BUF_MAX;
    use crate::fault_fs::FaultFs;
    use flux_fs::LinkKind;

    fn opts() -> VerifyOptions {
        VerifyOptions {
            links: Links::Copy,
            mounts: MountPolicy::Cross,
            hash: HashAlgorithm::Blake3,
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
        assert!(!fs.called("open_read(/d/a)"));
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
                (format!("sub/b.flux-partial.{ZEROS}").as_str(), b"p"),
            ],
        );
        fs.add_symlink_to(format!("/d/l.flux-partial.{ZEROS}"), "t", LinkKind::Unspecified);
        let (rows, s) = run(&fs, "/s", "/d", &opts());
        assert_eq!(
            rows,
            vec![
                rec(".flux-partial.txt", Outcome::Extra, Kind::File),
                rec("sub", Outcome::Match, Kind::Dir),
                rec("sub/.flux", Outcome::Extra, Kind::File),
                rec("x.flux-partial.1", Outcome::Extra, Kind::File),
            ]
        );
        assert_eq!((s.extra, s.matched), (3, 1));
        // The brief also asked for `!fs.called("read_dir(/d/.flux")`. `Walk::enter` lists a directory before it yields
        // the `Dir` event (`walk.rs:452`), so the subtree is dropped from the records but is still listed; asserting
        // otherwise needs a walker change (reported, not bent here).
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
}
