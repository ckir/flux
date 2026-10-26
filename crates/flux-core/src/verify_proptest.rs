// Copyright (c) 2026 Costas Kirgoussios
//
// This software is licensed under the PolyForm Noncommercial License 1.0.0.
// You may obtain a copy of the License at:
// https://polyformproject.org/licenses/noncommercial/1.0.0/

//! The verify-merge property test's case model: the generated `Case`, and the pure
//! derivation of the two trees (source and destination) it describes. Later pieces
//! (materialisation, the reference, the generator and the property) build on it.

use crate::fault_fs::FaultFs;
use crate::verify::{Kind, MOUNT_NOT_ENTERED, Outcome, Record, SPECIAL_NOT_COMPARED, Summary};
use crate::walk::MountPolicy;
use flux_fs::{Code, FileSystem, FluxPathKey, LinkKind, Links, MountRoot};
use proptest::collection::vec;
use proptest::prelude::{BoxedStrategy, Just, Strategy, any, prop_oneof};
use proptest::sample::select;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// A relative path as 1 to 4 components, joined with `Path::join`.
type RelPath = Vec<&'static str>;

/// Which side of the comparison an edit or extra applies to.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Which {
    Source,
    Dest,
}

/// What one tree entry is.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Spec {
    File { len: usize, fill: u8 },
    Dir { mount: MountRoot, list_error: Option<Code> },
    Symlink { payload: &'static str },
    Other,
}

/// How the destination (or source) entry at a base index differs from the base.
#[derive(Clone, Debug, PartialEq, Eq)]
#[allow(clippy::enum_variant_names)] // the variant names are the design's vocabulary
enum Edit {
    KeepBoth,
    EditContent(Which),
    ChangeKind(Which, Spec),
    DropFromDest,
    DropFromSource,
}

/// One generated scenario: a base tree, one edit per base entry, and per-side extras.
#[derive(Clone, Debug)]
struct Case {
    base: Vec<(RelPath, Spec)>,
    edits: Vec<Edit>,
    extras: Vec<(Which, RelPath, Spec)>,
    mounts: MountPolicy,
    links: Links,
}

fn join(rel: &RelPath) -> PathBuf {
    let mut it = rel.iter();
    let mut p = PathBuf::from(it.next().expect("a relative path has a component"));
    for c in it {
        p = p.join(c);
    }
    p
}

fn edit_content(spec: &Spec) -> Spec {
    match spec {
        Spec::File { len: 0, fill } => Spec::File { len: 1, fill: *fill },
        Spec::File { len, fill } => Spec::File { len: *len, fill: fill.wrapping_add(1) },
        Spec::Symlink { payload: "t" } => Spec::Symlink { payload: "u" },
        Spec::Symlink { .. } => Spec::Symlink { payload: "t" },
        other => other.clone(),
    }
}

/// Drop every entry whose parent is absent or not a directory on this side, shallow to deep.
fn prune(side: &mut BTreeMap<PathBuf, Spec>) {
    let mut paths: Vec<PathBuf> = side.keys().cloned().collect();
    paths.sort_by_key(|p| p.components().count());
    for p in paths {
        let parent = p.parent().filter(|q| !q.as_os_str().is_empty());
        let keep = match parent {
            None => true,
            Some(q) => matches!(side.get(q), Some(Spec::Dir { .. })),
        };
        if !keep {
            side.remove(&p);
        }
    }
}

/// Derive the (source, destination) trees of a case, as relative paths.
fn sides(case: &Case) -> (BTreeMap<PathBuf, Spec>, BTreeMap<PathBuf, Spec>) {
    let mut source = BTreeMap::new();
    let mut dest = BTreeMap::new();
    let mut seen = std::collections::BTreeSet::new();
    for (i, (rel, spec)) in case.base.iter().enumerate() {
        let path = join(rel);
        if !seen.insert(path.clone()) {
            continue;
        }
        source.insert(path.clone(), spec.clone());
        dest.insert(path.clone(), spec.clone());
        match &case.edits[i] {
            Edit::KeepBoth => {}
            Edit::EditContent(w) => {
                let side = if *w == Which::Source { &mut source } else { &mut dest };
                let edited = edit_content(&side[&path]);
                side.insert(path, edited);
            }
            Edit::ChangeKind(w, new) => {
                let side = if *w == Which::Source { &mut source } else { &mut dest };
                side.insert(path, new.clone());
            }
            Edit::DropFromDest => {
                dest.remove(&path);
            }
            Edit::DropFromSource => {
                source.remove(&path);
            }
        }
    }
    for (w, rel, spec) in &case.extras {
        let side = if *w == Which::Source { &mut source } else { &mut dest };
        side.entry(join(rel)).or_insert_with(|| spec.clone());
    }
    prune(&mut source);
    prune(&mut dest);
    (source, dest)
}

/// The `io::ErrorKind` that pairs with a fault code (the two fixed pairs); panics on any other code, because
/// the generator emits only those.
fn fault_kind(code: Code) -> std::io::ErrorKind {
    match code {
        Code::PermissionDenied => std::io::ErrorKind::PermissionDenied,
        Code::IoError => std::io::ErrorKind::Other,
        other => panic!("no fault pairing for {other:?}"),
    }
}

/// Build the fake filesystem a case describes, under `/s` and `/d`. Returns the fake and the number of calls
/// logged by the setup, so the property can ignore them by index.
fn materialise(case: &Case) -> (FaultFs, usize) {
    let fs = FaultFs::new();
    let (src, dst) = sides(case);
    for (root, side) in [("/s", &src), ("/d", &dst)] {
        let root = Path::new(root);
        fs.create_dir(root).unwrap();
        let mut dirs: Vec<(&PathBuf, &Spec)> =
            side.iter().filter(|(_, s)| matches!(s, Spec::Dir { .. })).collect();
        dirs.sort_by_key(|(p, _)| p.components().count());
        for (p, _) in &dirs {
            fs.create_dir(&root.join(p)).unwrap();
        }
        for (p, spec) in side {
            let at = root.join(p);
            match spec {
                Spec::File { len, fill } => fs.write_file(&at, &vec![*fill; *len]),
                Spec::Symlink { payload } => {
                    fs.add_symlink_to(&at, *payload, LinkKind::Unspecified)
                }
                Spec::Other => fs.add_special(&at),
                // Directories were created in the preceding pass.
                Spec::Dir { .. } => {}
            }
        }
        for (p, spec) in &dirs {
            if let Spec::Dir { mount, list_error } = spec {
                let at = root.join(p);
                if *mount != MountRoot::No {
                    fs.set_mount_root(&at, *mount);
                }
                if let Some(code) = list_error {
                    fs.fail_read_dir_of(&at, *code, fault_kind(*code));
                }
            }
        }
    }
    let n = fs.calls().len();
    (fs, n)
}

/// A record in the form the tests compare: the path's components joined with `/`.
type Row = (String, Outcome, Kind, Option<Code>, String);

fn row(r: &Record) -> Row {
    let path =
        r.path.components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/");
    (path, r.outcome, r.kind, r.code, r.detail.clone())
}

fn kind_of(spec: &Spec) -> Kind {
    match spec {
        Spec::File { .. } => Kind::File,
        Spec::Dir { .. } => Kind::Dir,
        Spec::Symlink { .. } => Kind::Symlink,
        Spec::Other => Kind::Other,
    }
}

type Keyed = BTreeMap<Vec<u8>, (PathBuf, Spec)>;

/// Whether a destination name is `<head>.flux-partial.<32 lowercase hex>` with a non-empty head.
fn is_partial_name(name: &str) -> bool {
    let Some(split) = name.len().checked_sub(32) else { return false };
    let (rest, id) = match (name.get(..split), name.get(split..)) {
        (Some(r), Some(i)) => (r, i),
        _ => return false,
    };
    id.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        && rest.strip_suffix(".flux-partial.").is_some_and(|head| !head.is_empty())
}

/// The destination's control state, which verify hides (design 6.5 step 2).
fn is_control(path: &Path, spec: &Spec) -> bool {
    let comps: Vec<_> = path.components().collect();
    let first = comps[0].as_os_str().to_string_lossy();
    if first == ".flux" && (comps.len() > 1 || matches!(spec, Spec::Dir { .. })) {
        return true;
    }
    if comps.len() == 1 && first == ".flux-root.lock" && matches!(spec, Spec::File { .. }) {
        return true;
    }
    let name = comps[comps.len() - 1].as_os_str().to_string_lossy();
    matches!(spec, Spec::File { .. } | Spec::Symlink { .. }) && is_partial_name(&name)
}

fn key_of(path: &Path) -> Vec<u8> {
    FluxPathKey::from_relative(path).unwrap().0
}

fn under(key: &[u8], pruned: &[u8]) -> bool {
    key == pruned || (key.starts_with(pruned) && key[pruned.len()] == 0x00)
}

fn digest_hex(len: usize, fill: u8) -> String {
    let mut h = flux_hash::Hasher::new();
    h.update(&vec![fill; len]);
    h.finalize().to_hex()
}

/// Steps 0 to 2 of the reference (design 6.5): the two trees as the walker and verify's filters leave them.
fn filtered_sides(case: &Case) -> (BTreeMap<PathBuf, Spec>, BTreeMap<PathBuf, Spec>) {
    let (mut src, mut dst) = sides(case);
    // Step 0: the walker view.
    if case.mounts == MountPolicy::Stay {
        for side in [&mut src, &mut dst] {
            let mounts: Vec<PathBuf> = side
                .iter()
                .filter(|(_, s)| matches!(s, Spec::Dir { mount: MountRoot::Yes, .. }))
                .map(|(p, _)| p.clone())
                .collect();
            side.retain(|p, _| !mounts.iter().any(|m| p.starts_with(m) && p != m));
        }
    }
    // Step 1: links skipped on the source.
    if case.links == Links::Skip {
        src.retain(|_, s| !matches!(s, Spec::Symlink { .. }));
    }
    // Step 2: destination control state.
    dst.retain(|p, s| !is_control(p, s));
    (src, dst)
}

/// The non-streaming reference for `verify_tree` (design 6.5): the records and the summary a case must produce.
fn reference(case: &Case) -> (Vec<Record>, Summary) {
    let (src, dst) = filtered_sides(case);
    let stay = case.mounts == MountPolicy::Stay;
    // Step 3: key both sides.
    let key = |side: BTreeMap<PathBuf, Spec>| -> Keyed {
        side.into_iter().map(|(p, s)| (key_of(&p), (p, s))).collect()
    };
    let (src, dst) = (key(src), key(dst));
    // Step 4: the merge.
    let failed = |s: &Spec| match s {
        Spec::Dir { mount, list_error: Some(code) } if !(stay && *mount == MountRoot::Yes) => {
            Some((*code, std::io::Error::from(fault_kind(*code)).to_string()))
        }
        _ => None,
    };
    let mut keys: Vec<&Vec<u8>> = src.keys().chain(dst.keys()).collect();
    keys.sort();
    keys.dedup();
    let mut pruned: Option<Vec<u8>> = None;
    let mut out = Vec::new();
    let mut sum = Summary::default();
    for k in keys {
        if pruned.as_ref().is_some_and(|p| under(k, p)) {
            continue;
        }
        let rec = |path: &Path, outcome, kind, code, detail: &str| Record {
            path: path.to_path_buf(),
            outcome,
            kind,
            code,
            detail: detail.to_string(),
        };
        let record = match (src.get(k), dst.get(k)) {
            (Some((p, s)), None) => match failed(s) {
                Some((code, text)) => {
                    pruned = Some(k.clone());
                    rec(p, Outcome::Unreadable, Kind::Dir, Some(code), &text)
                }
                None => rec(p, Outcome::Missing, kind_of(s), None, ""),
            },
            (None, Some((p, d))) => match failed(d) {
                Some((code, text)) => {
                    pruned = Some(k.clone());
                    rec(p, Outcome::Extra, Kind::Dir, Some(code), &text)
                }
                None => rec(p, Outcome::Extra, kind_of(d), None, ""),
            },
            (Some((p, s)), Some((_, d))) => {
                let (fs_, fd) = (failed(s), failed(d));
                if let Some((code, text)) = fs_.or(fd) {
                    pruned = Some(k.clone());
                    rec(p, Outcome::Unreadable, kind_of(s), Some(code), &text)
                } else {
                    match (s, d) {
                        (Spec::Dir { mount: ms, .. }, Spec::Dir { mount: md, .. }) => {
                            if stay && (*ms == MountRoot::Yes || *md == MountRoot::Yes) {
                                pruned = Some(k.clone());
                                sum.not_entered += 1;
                                rec(p, Outcome::Match, Kind::Dir, None, MOUNT_NOT_ENTERED)
                            } else {
                                rec(p, Outcome::Match, Kind::Dir, None, "")
                            }
                        }
                        (Spec::File { len: ls, fill: fls }, Spec::File { len: ld, fill: fld }) => {
                            sum.bytes_hashed += (*ls + *ld) as u64;
                            let (hs, hd) = (digest_hex(*ls, *fls), digest_hex(*ld, *fld));
                            if hs == hd {
                                rec(p, Outcome::Match, Kind::File, None, "")
                            } else {
                                let text = format!("{} != {}", &hs[..8], &hd[..8]);
                                rec(
                                    p,
                                    Outcome::Mismatched,
                                    Kind::File,
                                    Some(Code::VerifyMismatch),
                                    &text,
                                )
                            }
                        }
                        (Spec::Symlink { payload: a }, Spec::Symlink { payload: b }) => {
                            if a == b {
                                rec(p, Outcome::Match, Kind::Symlink, None, "")
                            } else {
                                rec(
                                    p,
                                    Outcome::Mismatched,
                                    Kind::Symlink,
                                    Some(Code::VerifyMismatch),
                                    "symlink payloads differ",
                                )
                            }
                        }
                        (Spec::Other, Spec::Other) => rec(
                            p,
                            Outcome::Unreadable,
                            Kind::Other,
                            Some(Code::SpecialFileUnsupported),
                            SPECIAL_NOT_COMPARED,
                        ),
                        _ => {
                            let (ks, kd) = (kind_of(s), kind_of(d));
                            let text =
                                format!("source {}, destination {}", ks.as_str(), kd.as_str());
                            rec(p, Outcome::Mismatched, ks, Some(Code::VerifyMismatch), &text)
                        }
                    }
                }
            }
            (None, None) => unreachable!("a key comes from one of the sides"),
        };
        match record.outcome {
            Outcome::Match => sum.matched += 1,
            Outcome::Missing => sum.missing += 1,
            Outcome::Mismatched => sum.mismatched += 1,
            Outcome::Extra => sum.extra += 1,
            Outcome::Unreadable => sum.unreadable += 1,
        }
        out.push(record);
    }
    (out, sum)
}

// ---- The case generator (design 6.3) ----

const LOCK: &str = ".flux-root.lock";
/// The ordinary name vocabulary; `.flux-root.lock` joins it only as a `File` name (see `entry`).
const PLAIN_NAMES: [&str; 7] = ["a", "a-b", "a.txt", "a0", "b", "sub", "z"];
/// Controlled hex ids, so a partial-file name stays a `&'static str`.
macro_rules! partial {
    ($head:literal, $id:literal) => {
        concat!($head, ".flux-partial.", $id)
    };
}
const PARTIAL_32: [&str; 3] = [
    partial!("a", "0123456789abcdef0123456789abcdef"),
    partial!("b", "0123456789abcdef0123456789abcdef"),
    partial!("sub", "0123456789abcdef0123456789abcdef"),
];
const PARTIAL_31: [&str; 3] = [
    partial!("a", "0123456789abcdef0123456789abcde"),
    partial!("b", "0123456789abcdef0123456789abcde"),
    partial!("sub", "0123456789abcdef0123456789abcde"),
];
const PARTIAL_UPPER: [&str; 3] = [
    partial!("a", "0123456789ABCDEF0123456789ABCDEF"),
    partial!("b", "0123456789ABCDEF0123456789ABCDEF"),
    partial!("sub", "0123456789ABCDEF0123456789ABCDEF"),
];
const PARTIAL_NO_HEAD: &str = ".flux-partial.0123456789abcdef0123456789abcdef";
/// How many control-state shapes `control` knows: ten explicit arms (0 to 9); the `_` arm, the nested `.flux`
/// directory, takes the other two values (10 and 11).
const CONTROL_SHAPES: usize = 12;

fn which() -> impl Strategy<Value = Which> {
    prop_oneof![Just(Which::Source), Just(Which::Dest)]
}

fn file_spec() -> impl Strategy<Value = Spec> {
    // The large file is one in eight.
    let len = prop_oneof![
        1 => Just(crate::copy::COPY_BUF_MAX + 1),
        7 => select(&[0usize, 1, 17][..]),
    ];
    (len, 0u8..4).prop_map(|(len, fill)| Spec::File { len, fill })
}

fn dir_spec() -> impl Strategy<Value = Spec> {
    let mount = prop_oneof![
        1 => Just(MountRoot::Yes),
        2 => Just(MountRoot::No),
        1 => Just(MountRoot::Unknown),
    ];
    let list_error = prop_oneof![
        3 => Just(None),
        1 => select(&[Code::PermissionDenied, Code::IoError][..]).prop_map(Some),
    ];
    (mount, list_error).prop_map(|(mount, list_error)| Spec::Dir { mount, list_error })
}

fn symlink_spec() -> impl Strategy<Value = Spec> {
    select(&["t", "u"][..]).prop_map(|payload| Spec::Symlink { payload })
}

fn spec() -> impl Strategy<Value = Spec> {
    prop_oneof![
        8 => file_spec(),
        30 => dir_spec(),
        3 => symlink_spec(),
        1 => Just(Spec::Other),
    ]
}

/// A name with the spec it may carry: the lock name is only ever a `File`.
fn entry() -> impl Strategy<Value = (&'static str, Spec)> {
    prop_oneof![
        15 => (select(&PLAIN_NAMES[..]), spec()),
        1 => (Just(LOCK), file_spec()),
    ]
}

/// Place raw entries into a base: directories first, each entry under the root or an earlier directory.
fn place(mut raw: Vec<(usize, (&'static str, Spec))>) -> Vec<(RelPath, Spec)> {
    raw.sort_by_key(|(_, (_, spec))| !matches!(spec, Spec::Dir { .. }));
    let mut dirs: Vec<RelPath> = Vec::new();
    let mut base = Vec::new();
    for (pick, (name, spec)) in raw {
        let slot = pick % (dirs.len() + 1);
        let mut rel = if slot == 0 { Vec::new() } else { dirs[slot - 1].clone() };
        rel.push(name);
        if matches!(spec, Spec::Dir { .. }) && rel.len() < 4 {
            dirs.push(rel.clone());
        }
        base.push((rel, spec));
    }
    base
}

fn base() -> impl Strategy<Value = Vec<(RelPath, Spec)>> {
    vec((any::<usize>(), entry()), 3..=10).prop_map(place)
}

/// One edit; a lock-named entry may only ever change into a `File` (the name is a `File` name only).
fn edit(lock_named: bool) -> impl Strategy<Value = Edit> {
    let target = if lock_named { file_spec().boxed() } else { spec().boxed() };
    prop_oneof![
        8 => Just(Edit::KeepBoth),
        1 => which().prop_map(Edit::EditContent),
        4 => (which(), target).prop_map(|(w, s)| Edit::ChangeKind(w, s)),
        2 => Just(Edit::DropFromDest),
        1 => Just(Edit::DropFromSource),
    ]
}

/// One edit per base entry, each aware of its entry's name.
fn edits_for(base: &[(RelPath, Spec)]) -> Vec<BoxedStrategy<Edit>> {
    base.iter().map(|(rel, _)| edit(rel.last() == Some(&LOCK)).boxed()).collect()
}

/// Where an extra may go: the root (the empty path) or any base `Dir` with room for a child.
fn parents_of(base: &[(RelPath, Spec)]) -> Vec<RelPath> {
    let mut parents: Vec<RelPath> = vec![Vec::new()];
    for (rel, spec) in base {
        if matches!(spec, Spec::Dir { .. }) && rel.len() < 4 && !parents.contains(rel) {
            parents.push(rel.clone());
        }
    }
    parents
}

fn child(parent: &RelPath, name: &'static str) -> RelPath {
    let mut rel = parent.clone();
    rel.push(name);
    rel
}

type Extra = (Which, RelPath, Spec);

/// An ordinary extra: either side, an ordinary name, any spec.
fn ordinary_extra(parents: Vec<RelPath>) -> impl Strategy<Value = Vec<Extra>> {
    (which(), select(parents), entry())
        .prop_map(|(w, parent, (name, spec))| vec![(w, child(&parent, name), spec)])
}

/// A control-state extra on the destination: the shapes verify must hide, and their near misses.
fn control(parent: &RelPath, head: usize, shape: usize) -> Vec<Extra> {
    let f = Spec::File { len: 1, fill: 9 };
    let d = Spec::Dir { mount: MountRoot::No, list_error: None };
    let l = Spec::Symlink { payload: "t" };
    let at = |name| child(parent, name);
    let dest = |rel, spec| (Which::Dest, rel, spec);
    match shape % CONTROL_SHAPES {
        0 => vec![dest(vec![".flux"], d), dest(vec![".flux", "c"], f)],
        1 => vec![dest(vec![".flux"], f)],
        2 => vec![dest(vec![LOCK], f)],
        3 => vec![dest(vec![LOCK], d)],
        4 => vec![dest(at(PARTIAL_32[head % 3]), f)],
        5 => vec![dest(at(PARTIAL_32[head % 3]), l)],
        6 => vec![dest(at(PARTIAL_31[head % 3]), f)],
        7 => vec![dest(at(PARTIAL_UPPER[head % 3]), f)],
        8 => vec![dest(at(PARTIAL_NO_HEAD), f)],
        9 => vec![dest(at(PARTIAL_32[head % 3]), d)],
        _ => vec![dest(at(".flux"), d)],
    }
}

fn control_extra(parents: Vec<RelPath>) -> impl Strategy<Value = Vec<Extra>> {
    (select(parents), 0usize..3, 0..CONTROL_SHAPES)
        .prop_map(|(parent, head, shape)| control(&parent, head, shape))
}

/// 0 to 4 extras, the ordinary and the control-state kind equally likely.
fn extras(parents: Vec<RelPath>) -> impl Strategy<Value = Vec<Extra>> {
    let one = prop_oneof![ordinary_extra(parents.clone()), control_extra(parents)];
    vec(one, 0..=4).prop_map(|groups| {
        let mut all: Vec<Extra> = groups.into_iter().flatten().collect();
        all.truncate(4);
        all
    })
}

fn mounts() -> impl Strategy<Value = MountPolicy> {
    prop_oneof![Just(MountPolicy::Stay), Just(MountPolicy::Cross)]
}

fn links() -> impl Strategy<Value = Links> {
    prop_oneof![Just(Links::Copy), Just(Links::Skip)]
}

/// The generated scenario (design 6.3): a base tree, one edit per base entry, and per-side extras.
fn case() -> impl Strategy<Value = Case> {
    base()
        .prop_flat_map(|base| {
            let edits = edits_for(&base);
            let parents = parents_of(&base);
            (Just(base), edits, extras(parents), mounts(), links())
        })
        .prop_map(|(base, edits, extras, mounts, links)| Case {
            base,
            edits,
            extras,
            mounts,
            links,
        })
}

/// A relative path rendered with `/` separators on every platform.
fn slashed(path: &Path) -> String {
    path.components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/")
}

/// The case as a pasteable `verify::tests` fixture: one block per side, then the options.
fn fixture_snippet(case: &Case) -> String {
    use std::fmt::Write;
    let (src, dst) = sides(case);
    let mut out = String::new();
    for (root, side) in [("/s", &src), ("/d", &dst)] {
        let dirs: Vec<String> = side
            .iter()
            .filter(|(_, s)| matches!(s, Spec::Dir { .. }))
            .map(|(p, _)| format!("{:?}", slashed(p)))
            .collect();
        let files: Vec<String> = side
            .iter()
            .filter_map(|(p, s)| match s {
                Spec::File { len, fill } => Some(format!(
                    "({:?}, &vec![b'{}'; {len}])",
                    slashed(p),
                    std::ascii::escape_default(*fill)
                )),
                _ => None,
            })
            .collect();
        let _ = writeln!(
            out,
            "tree(&fs, \"{root}\", &[{}], &[{}]);",
            dirs.join(", "),
            files.join(", ")
        );
        for (p, spec) in side {
            let at = format!("{root}/{}", slashed(p));
            match spec {
                Spec::Symlink { payload } => {
                    let _ = writeln!(
                        out,
                        "fs.add_symlink_to(\"{at}\", \"{payload}\", LinkKind::Unspecified);"
                    );
                }
                Spec::Other => {
                    let _ = writeln!(out, "fs.add_special(\"{at}\");");
                }
                Spec::Dir { mount, list_error } => {
                    if *mount != MountRoot::No {
                        let _ = writeln!(out, "fs.set_mount_root(\"{at}\", MountRoot::{mount:?});");
                    }
                    if let Some(code) = list_error {
                        let _ = writeln!(
                            out,
                            "fs.fail_read_dir_of(\"{at}\", Code::{code:?}, std::io::ErrorKind::{:?});",
                            fault_kind(*code)
                        );
                    }
                }
                Spec::File { .. } => {}
            }
        }
    }
    let _ = writeln!(
        out,
        "let o = VerifyOptions {{ links: Links::{:?}, mounts: MountPolicy::{:?}, hash: HashAlgorithm::Blake3 }};",
        case.links, case.mounts
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sides_applies_edits_extras_dedupe_and_parent_pruning() {
        let dir = Spec::Dir { mount: MountRoot::No, list_error: None };
        let f = |len, fill| Spec::File { len, fill };
        let link = |p| Spec::Symlink { payload: p };
        let case = Case {
            base: vec![
                (vec!["a"], dir.clone()),
                (vec!["a", "x"], dir.clone()),
                (vec!["a", "x", "y"], f(1, b'y')),
                (vec!["b"], f(0, b'b')),
                (vec!["b"], f(1, b'B')),
                (vec!["l"], link("t")),
                (vec!["o"], Spec::Other),
            ],
            edits: vec![
                Edit::ChangeKind(Which::Dest, f(1, b'a')),
                Edit::KeepBoth,
                Edit::KeepBoth,
                Edit::EditContent(Which::Source),
                Edit::DropFromDest,
                Edit::EditContent(Which::Dest),
                Edit::EditContent(Which::Source),
            ],
            extras: vec![
                (Which::Dest, vec!["a", "z"], f(1, b'z')),
                (Which::Source, vec!["c"], dir.clone()),
                (Which::Source, vec!["b"], f(9, b'!')),
            ],
            mounts: MountPolicy::Cross,
            links: Links::Copy,
        };
        let (s, d) = sides(&case);
        let keys = |m: &BTreeMap<PathBuf, Spec>| {
            m.keys().map(|p| p.to_string_lossy().replace('\\', "/")).collect::<Vec<_>>()
        };
        assert_eq!(keys(&s), ["a", "a/x", "a/x/y", "b", "c", "l", "o"]);
        assert_eq!(keys(&d), ["a", "b", "l", "o"]);
        assert_eq!(s[&PathBuf::from("b")], f(1, b'b')); // EditContent on an empty file: len 1, fill unchanged
        assert_eq!(d[&PathBuf::from("b")], f(0, b'b')); // the dedupe loser's DropFromDest was ignored
        assert_eq!(d[&PathBuf::from("a")], f(1, b'a')); // ChangeKind on dest; a/x, a/x/y, a/z pruned there
        assert_eq!(d[&PathBuf::from("l")], link("u")); // EditContent on a symlink swaps the payload
        assert_eq!(s[&PathBuf::from("o")], Spec::Other); // EditContent is a no-op on Other
        assert_eq!(s[&PathBuf::from("b")].clone(), f(1, b'b')); // the extra at an existing path was dropped
    }

    fn file(len: usize, fill: u8) -> Spec {
        Spec::File { len, fill }
    }

    fn dir() -> Spec {
        Spec::Dir { mount: MountRoot::No, list_error: None }
    }

    fn case_of(
        entries: Vec<(RelPath, Spec, Edit)>,
        extras: Vec<(Which, RelPath, Spec)>,
        mounts: MountPolicy,
    ) -> Case {
        let (base, edits) = entries.into_iter().map(|(r, s, e)| ((r, s), e)).unzip();
        Case { base, edits, extras, mounts, links: Links::Copy }
    }

    fn rows_of(case: &Case) -> Vec<Row> {
        reference(case).0.iter().map(row).collect()
    }

    /// The three fixture cases of test (a), with the rows their `verify::tests` fixtures assert.
    fn fixture_cases() -> Vec<(Case, Vec<Row>)> {
        let mm = |p: &str, k: Kind, d: &str| {
            (p.to_string(), Outcome::Mismatched, k, Some(Code::VerifyMismatch), d.to_string())
        };
        let rec = |p: &str, o: Outcome, k: Kind| (p.to_string(), o, k, None, String::new());
        let drop_d = Edit::DropFromDest;
        let drop_s = Edit::DropFromSource;
        // a_type_mismatch_drains_the_children_as_extra_or_missing
        let type_mismatch = case_of(
            vec![
                (vec!["a"], dir(), Edit::ChangeKind(Which::Dest, file(1, b'a'))),
                (vec!["a", "x"], file(1, b'x'), drop_d.clone()),
                (vec!["b"], file(1, b'b'), Edit::ChangeKind(Which::Dest, dir())),
                (vec!["b", "y"], file(1, b'y'), drop_s.clone()),
            ],
            vec![],
            MountPolicy::Cross,
        );
        let type_rows = vec![
            mm("a", Kind::Dir, "source dir, destination file"),
            rec("a/x", Outcome::Missing, Kind::File),
            mm("b", Kind::File, "source file, destination dir"),
            rec("b/y", Outcome::Extra, Kind::File),
        ];
        // a_mount_point_on_either_side_is_matched_once_and_pruned, first half: destination mount, source child.
        let mount = |m: MountRoot| Spec::Dir { mount: m, list_error: None };
        let want =
            vec![("m".to_string(), Outcome::Match, Kind::Dir, None, MOUNT_NOT_ENTERED.to_string())];
        let dest_mount = case_of(
            vec![
                (vec!["m"], dir(), Edit::ChangeKind(Which::Dest, mount(MountRoot::Yes))),
                (vec!["m", "x"], file(1, b'x'), drop_d.clone()),
            ],
            vec![],
            MountPolicy::Stay,
        );
        // second half: source mount, destination child.
        let source_mount = case_of(
            vec![
                (vec!["m"], dir(), Edit::ChangeKind(Which::Source, mount(MountRoot::Yes))),
                (vec!["m", "x"], file(1, b'x'), drop_s.clone()),
            ],
            vec![],
            MountPolicy::Stay,
        );
        // a_destination_only_directory_that_cannot_be_listed_is_extra_with_its_error_and_never_fails
        let lost = Spec::Dir { mount: MountRoot::No, list_error: Some(Code::PermissionDenied) };
        let unlistable = case_of(
            vec![
                (vec!["a"], file(1, b'a'), Edit::KeepBoth),
                (vec!["lost"], lost, drop_s.clone()),
                (vec!["lost", "x"], file(1, b'x'), drop_s.clone()),
                (vec!["z"], file(1, b'z'), drop_s),
            ],
            vec![],
            MountPolicy::Cross,
        );
        let unlistable_rows = vec![
            rec("a", Outcome::Match, Kind::File),
            (
                "lost".to_string(),
                Outcome::Extra,
                Kind::Dir,
                Some(Code::PermissionDenied),
                String::new(),
            ),
            rec("z", Outcome::Extra, Kind::File),
        ];
        vec![
            (type_mismatch, type_rows),
            (dest_mount, want.clone()),
            (source_mount, want),
            (unlistable, unlistable_rows),
        ]
    }

    /// Rows with the error text dropped, for the fixture whose text is platform-dependent.
    fn no_text(rows: &[Row]) -> Vec<(String, Outcome, Kind, Option<Code>)> {
        rows.iter().map(|r| (r.0.clone(), r.1, r.2, r.3)).collect()
    }

    #[test]
    fn the_reference_agrees_with_three_fixtures() {
        for (i, (case, want)) in fixture_cases().into_iter().enumerate() {
            let got = rows_of(&case);
            if i == 3 {
                assert_eq!(no_text(&got), no_text(&want), "{got:?}");
                assert_eq!(
                    got[1].4,
                    std::io::Error::from(std::io::ErrorKind::PermissionDenied).to_string()
                );
                let (_, sum) = reference(&case);
                assert_eq!((sum.extra, sum.unreadable, sum.matched), (2, 0, 1));
            } else {
                assert_eq!(got, want, "case {i}");
            }
        }
        let (_, sum) = reference(&fixture_cases()[1].0);
        assert_eq!((sum.matched, sum.not_entered, sum.missing), (1, 1, 0));
    }

    #[test]
    fn the_reference_hides_a_mount_roots_children_on_its_side_under_stay() {
        let mount = Spec::Dir { mount: MountRoot::Yes, list_error: None };
        let case = case_of(
            vec![
                (vec!["m"], file(1, b'm'), Edit::ChangeKind(Which::Dest, mount)),
                (vec!["m", "x"], file(1, b'x'), Edit::DropFromSource),
            ],
            vec![],
            MountPolicy::Stay,
        );
        let (records, sum) = reference(&case);
        let rows: Vec<Row> = records.iter().map(row).collect();
        assert_eq!(
            rows,
            [(
                "m".to_string(),
                Outcome::Mismatched,
                Kind::File,
                Some(Code::VerifyMismatch),
                "source file, destination dir".to_string()
            )]
        );
        assert_eq!(sum.not_entered, 0);
    }

    #[test]
    fn the_reference_gives_a_failed_pair_the_source_kind() {
        let bad = Spec::Dir { mount: MountRoot::No, list_error: Some(Code::IoError) };
        let case = case_of(
            vec![(vec!["p"], file(1, b'p'), Edit::ChangeKind(Which::Dest, bad))],
            vec![],
            MountPolicy::Cross,
        );
        assert_eq!(
            rows_of(&case),
            [(
                "p".to_string(),
                Outcome::Unreadable,
                Kind::File,
                Some(Code::IoError),
                "other error".to_string()
            )]
        );
    }

    #[test]
    fn an_empty_pair_of_trees_gives_no_records_and_a_default_summary() {
        use crate::verify::{VerifyOptions, verify_tree};
        use flux_fs::HashAlgorithm;
        let case = Case {
            base: vec![],
            edits: vec![],
            extras: vec![],
            mounts: MountPolicy::Cross,
            links: Links::Copy,
        };
        assert_eq!(reference(&case), (vec![], Summary::default()));
        let (fs, _) = materialise(&case);
        let opts =
            VerifyOptions { links: case.links, mounts: case.mounts, hash: HashAlgorithm::Blake3 };
        let mut records = Vec::new();
        let summary =
            verify_tree(&fs, Path::new("/s"), Path::new("/d"), &opts, &mut |r| records.push(r))
                .unwrap();
        assert_eq!((records, summary), (vec![], Summary::default()));
    }

    #[test]
    fn fixture_snippet_renders_every_kind_of_line() {
        let case = case_of(
            vec![
                (
                    vec!["d"],
                    Spec::Dir { mount: MountRoot::Yes, list_error: Some(Code::IoError) },
                    Edit::KeepBoth,
                ),
                (vec!["d", "f"], file(crate::copy::COPY_BUF_MAX + 1, b'q'), Edit::KeepBoth),
                (vec!["l"], Spec::Symlink { payload: "t" }, Edit::KeepBoth),
                (vec!["o"], Spec::Other, Edit::KeepBoth),
            ],
            vec![],
            MountPolicy::Stay,
        );
        let mut case = case;
        case.links = Links::Skip;
        let text = fixture_snippet(&case);
        for want in [
            "tree(&fs, \"/s\", &[\"d\"], &[(\"d/f\", &vec![b'q'; 262145])]);",
            "tree(&fs, \"/d\", &[\"d\"], &[(\"d/f\", &vec![b'q'; 262145])]);",
            "fs.add_symlink_to(\"/s/l\", \"t\", LinkKind::Unspecified);",
            "fs.add_special(\"/s/o\");",
            "fs.set_mount_root(\"/s/d\", MountRoot::Yes);",
            "fs.fail_read_dir_of(\"/s/d\", Code::IoError, std::io::ErrorKind::Other);",
            "let o = VerifyOptions { links: Links::Skip, mounts: MountPolicy::Stay, hash: HashAlgorithm::Blake3 };",
        ] {
            assert!(text.contains(want), "missing {want:?} in:\n{text}");
        }
    }

    #[test]
    fn materialise_builds_what_sides_describe() {
        use crate::verify::{VerifyOptions, verify_tree};
        use flux_fs::HashAlgorithm;
        for (i, (case, _)) in fixture_cases().into_iter().enumerate() {
            let (fs, _setup) = materialise(&case);
            let opts = VerifyOptions {
                links: case.links,
                mounts: case.mounts,
                hash: HashAlgorithm::Blake3,
            };
            let mut rows = Vec::new();
            verify_tree(&fs, Path::new("/s"), Path::new("/d"), &opts, &mut |r| rows.push(row(&r)))
                .unwrap();
            assert_eq!(rows, rows_of(&case), "case {i}");
        }
    }

    /// Every category the census counts, in report order.
    const CATEGORIES: [&str; 25] = [
        "outcome match",
        "outcome missing",
        "outcome mismatched",
        "outcome extra",
        "outcome unreadable",
        "source-only failed dir",
        "destination-only failed dir",
        "both-sides failed dir, different codes",
        "fault on exactly one side of a shared path",
        "mount on exactly one side of a shared path",
        "failed destination dir over a source file",
        "mount fold (not_entered > 0)",
        "mount dir under Stay with children on its side",
        "pruned dir then a prefix-named later sibling",
        "two prunes each with a surviving descendant",
        "control-state drop",
        "ChangeKind on both Source and Dest",
        "subtree on one side under a non-directory on the other",
        "source symlink removed by Skip",
        "source .flux-root.lock file reported Missing",
        "symlink on both sides reported Extra under Skip",
        "mount dir with a fault under Cross reported failed",
        "case with a large file",
        "case with a Dir under a Dir",
        "case with a source-side extra",
    ];

    fn is_fail(code: Option<Code>) -> bool {
        matches!(code, Some(Code::PermissionDenied | Code::IoError))
    }

    fn dir_err(spec: Option<&Spec>) -> Option<Code> {
        match spec {
            Some(Spec::Dir { list_error, .. }) => *list_error,
            _ => None,
        }
    }

    fn is_mount(spec: &Spec) -> bool {
        matches!(spec, Spec::Dir { mount: MountRoot::Yes, .. })
    }

    /// The categories a case reaches, read from the case, `sides` and the reference only (never the engine).
    fn categories(case: &Case) -> Vec<&'static str> {
        let (src, dst) = sides(case);
        let (fsrc, fdst) = filtered_sides(case);
        let (recs, sum) = reference(case);
        let stay = case.mounts == MountPolicy::Stay;
        let has_record = |p: &Path| recs.iter().any(|r| r.path == p);
        let mut hit = Vec::new();
        for (name, outcome) in [
            ("outcome match", Outcome::Match),
            ("outcome missing", Outcome::Missing),
            ("outcome mismatched", Outcome::Mismatched),
            ("outcome extra", Outcome::Extra),
            ("outcome unreadable", Outcome::Unreadable),
        ] {
            if recs.iter().any(|r| r.outcome == outcome) {
                hit.push(name);
            }
        }
        let mut when = |name: &'static str, yes: bool| {
            if yes {
                hit.push(name);
            }
        };
        when(
            "source-only failed dir",
            recs.iter().any(|r| {
                r.outcome == Outcome::Unreadable
                    && is_fail(r.code)
                    && dir_err(fsrc.get(&r.path)).is_some()
                    && !fdst.contains_key(&r.path)
            }),
        );
        when(
            "destination-only failed dir",
            recs.iter().any(|r| {
                r.outcome == Outcome::Extra
                    && is_fail(r.code)
                    && dir_err(fdst.get(&r.path)).is_some()
                    && !fsrc.contains_key(&r.path)
            }),
        );
        when(
            "both-sides failed dir, different codes",
            recs.iter().any(|r| {
                let (s, d) = (dir_err(fsrc.get(&r.path)), dir_err(fdst.get(&r.path)));
                r.outcome == Outcome::Unreadable
                    && s.is_some()
                    && d.is_some()
                    && r.code == s
                    && s != d
            }),
        );
        let shared = || src.iter().filter_map(|(p, s)| dst.get(p).map(|d| (p, s, d)));
        when(
            "fault on exactly one side of a shared path",
            shared().any(|(p, s, d)| {
                matches!(s, Spec::Dir { .. })
                    && matches!(d, Spec::Dir { .. })
                    && dir_err(Some(s)).is_some() != dir_err(Some(d)).is_some()
                    && has_record(p)
            }),
        );
        when(
            "mount on exactly one side of a shared path",
            shared().any(|(p, s, d)| {
                matches!(s, Spec::Dir { .. })
                    && matches!(d, Spec::Dir { .. })
                    && is_mount(s) != is_mount(d)
                    && has_record(p)
            }),
        );
        when(
            "failed destination dir over a source file",
            recs.iter().any(|r| {
                r.outcome == Outcome::Unreadable
                    && r.kind == Kind::File
                    && is_fail(r.code)
                    && matches!(fsrc.get(&r.path), Some(Spec::File { .. }))
                    && dir_err(fdst.get(&r.path)).is_some()
            }),
        );
        when("mount fold (not_entered > 0)", sum.not_entered > 0);
        when(
            "mount dir under Stay with children on its side",
            stay && [&src, &dst].iter().any(|side| {
                side.iter()
                    .any(|(p, s)| is_mount(s) && side.keys().any(|q| q != p && q.starts_with(p)))
            }),
        );
        let prunes: Vec<&Record> = recs
            .iter()
            .filter(|r| {
                (matches!(r.outcome, Outcome::Unreadable | Outcome::Extra) && is_fail(r.code))
                    || (r.outcome == Outcome::Match && r.detail == MOUNT_NOT_ENTERED)
            })
            .collect();
        when(
            "pruned dir then a prefix-named later sibling",
            prunes.iter().any(|p| {
                recs.iter().any(|q| {
                    q.path != p.path
                        && q.path.parent() == p.path.parent()
                        && key_of(&q.path).starts_with(&key_of(&p.path))
                })
            }),
        );
        when(
            "two prunes each with a surviving descendant",
            prunes
                .iter()
                .filter(|p| {
                    fsrc.keys().chain(fdst.keys()).any(|q| q != &p.path && q.starts_with(&p.path))
                })
                .count()
                >= 2,
        );
        when(
            "control-state drop",
            case.extras.iter().any(|(w, rel, spec)| {
                let path = join(rel);
                *w == Which::Dest
                    && is_control(&path, spec)
                    && dst.get(&path) == Some(spec)
                    && !fdst.contains_key(&path)
            }),
        );
        let mut seen = std::collections::BTreeSet::new();
        let (mut change_src, mut change_dst) = (false, false);
        for (i, (rel, spec)) in case.base.iter().enumerate() {
            if !seen.insert(join(rel)) {
                continue;
            }
            if let Edit::ChangeKind(w, new) = &case.edits[i]
                && kind_of(new) != kind_of(spec)
            {
                let path = join(rel);
                match w {
                    Which::Source if src.get(&path) == Some(new) => change_src = true,
                    Which::Dest if dst.get(&path) == Some(new) => change_dst = true,
                    _ => {}
                }
            }
        }
        when("ChangeKind on both Source and Dest", change_src && change_dst);
        let under_non_dir = |a: &BTreeMap<PathBuf, Spec>, b: &BTreeMap<PathBuf, Spec>| {
            a.keys().any(|p| {
                p.parent()
                    .filter(|q| !q.as_os_str().is_empty())
                    .is_some_and(|q| b.get(q).is_some_and(|s| !matches!(s, Spec::Dir { .. })))
            })
        };
        when(
            "subtree on one side under a non-directory on the other",
            under_non_dir(&src, &dst) || under_non_dir(&dst, &src),
        );
        when(
            "source symlink removed by Skip",
            case.links == Links::Skip && src.values().any(|s| matches!(s, Spec::Symlink { .. })),
        );
        when(
            "source .flux-root.lock file reported Missing",
            src.iter().any(|(p, s)| {
                p == Path::new(".flux-root.lock")
                    && matches!(s, Spec::File { .. })
                    && recs.iter().any(|r| r.path == *p && r.outcome == Outcome::Missing)
            }),
        );
        when(
            "symlink on both sides reported Extra under Skip",
            case.links == Links::Skip
                && shared().any(|(p, s, d)| {
                    matches!(s, Spec::Symlink { .. })
                        && matches!(d, Spec::Symlink { .. })
                        && recs.iter().any(|r| r.path == *p && r.outcome == Outcome::Extra)
                }),
        );
        when(
            "mount dir with a fault under Cross reported failed",
            case.mounts == MountPolicy::Cross
                && [&src, &dst].iter().any(|side| {
                    side.iter().any(|(p, s)| {
                        matches!(s, Spec::Dir { mount: MountRoot::Yes, list_error: Some(c) }
                            if recs.iter().any(|r| r.path == *p && r.code == Some(*c)))
                    })
                }),
        );
        let large = crate::copy::COPY_BUF_MAX + 1;
        when(
            "case with a large file",
            src.values()
                .chain(dst.values())
                .any(|s| matches!(s, Spec::File { len, .. } if *len == large)),
        );
        when(
            "case with a Dir under a Dir",
            src.iter().any(|(p, s)| {
                matches!(s, Spec::Dir { .. })
                    && p.parent().is_some_and(|q| !q.as_os_str().is_empty())
            }),
        );
        when("case with a source-side extra", case.extras.iter().any(|e| e.0 == Which::Source));
        hit
    }

    #[test]
    fn generated_cases_reach_every_outcome_and_every_prune_arm() {
        use proptest::strategy::ValueTree;
        use proptest::test_runner::{Config, RngAlgorithm, TestRng, TestRunner};
        let rng = TestRng::from_seed(RngAlgorithm::ChaCha, &[7u8; 32]);
        let mut runner = TestRunner::new_with_rng(Config::default(), rng);
        let strategy = case();
        let mut counts: BTreeMap<&str, usize> = CATEGORIES.iter().map(|c| (*c, 0)).collect();
        for _ in 0..512 {
            let c = strategy.new_tree(&mut runner).unwrap().current();
            for name in categories(&c) {
                *counts.get_mut(name).unwrap_or_else(|| panic!("unknown category {name}")) += 1;
            }
        }
        let table: String =
            CATEGORIES.iter().map(|c| format!("  {:>4}  {c}\n", counts[c])).collect();
        println!("census over 512 cases:\n{table}");
        for c in CATEGORIES {
            assert!(
                counts[c] >= 5,
                "category {c:?} reached {} of 512 cases; census:\n{table}",
                counts[c]
            );
        }
    }

    proptest::proptest! {
        #![proptest_config(proptest::prelude::ProptestConfig::default())]

        #[test]
        fn the_streaming_merge_equals_the_reference(case in case()) {
            use crate::verify::{VerifyOptions, verify_tree};
            use flux_fs::HashAlgorithm;
            use proptest::prop_assert;
            use proptest::prop_assert_eq;
            let (fs, setup) = materialise(&case);
            let opts = VerifyOptions {
                links: case.links,
                mounts: case.mounts,
                hash: HashAlgorithm::Blake3,
            };
            let mut rows = Vec::new();
            let result =
                verify_tree(&fs, Path::new("/s"), Path::new("/d"), &opts, &mut |r| rows.push(r));
            prop_assert!(
                result.is_ok(),
                "verify_tree failed: {:?}\n{}",
                result.as_ref().err(),
                fixture_snippet(&case)
            );
            let summary = result.unwrap();
            let (expected_rows, expected_summary) = reference(&case);
            prop_assert_eq!(&rows, &expected_rows, "{}", fixture_snippet(&case));
            prop_assert_eq!(summary, expected_summary, "{}", fixture_snippet(&case));
            let keys: Vec<Vec<u8>> =
                rows.iter().map(|r| FluxPathKey::from_relative(&r.path).unwrap().0).collect();
            prop_assert!(
                keys.windows(2).all(|w| w[0] < w[1]),
                "keys not strictly increasing: {keys:?}\n{}",
                fixture_snippet(&case)
            );
            let (src, dst) = sides(&case);
            let calls = fs.calls();
            for call in calls.iter().map(String::as_str) {
                let Some(rest) = call.strip_prefix("open_read(") else { continue };
                let rest = rest.strip_suffix(')').unwrap_or(rest).to_owned();
                let tail = rest.strip_prefix("/s/").or_else(|| rest.strip_prefix("/d/"));
                prop_assert!(
                    tail.is_some_and(|tail| {
                        let rel = tail.split('/').fold(PathBuf::new(), |p, c| p.join(c));
                        matches!(src.get(&rel), Some(Spec::File { .. }))
                            && matches!(dst.get(&rel), Some(Spec::File { .. }))
                    }),
                    "open_read of a path that is not a file on both sides, or a root: {call}\n{}",
                    fixture_snippet(&case)
                );
            }
            const READS: [&str; 7] = [
                "metadata(",
                "read_dir(",
                "open_read(",
                "read_link(",
                "source_mount_root(",
                "canonical_path(",
                "identity(",
            ];
            for call in calls.iter().skip(setup) {
                prop_assert!(
                    READS.iter().any(|r| call.starts_with(r)),
                    "a non-read call: {call}\n{}",
                    fixture_snippet(&case)
                );
            }
            if case.mounts == MountPolicy::Stay {
                for (root, side) in [("/s", &src), ("/d", &dst)] {
                    for (p, spec) in side {
                        if !matches!(spec, Spec::Dir { mount: MountRoot::Yes, .. }) {
                            continue;
                        }
                        let listing = format!(
                            "read_dir({})",
                            Path::new(root).join(p).to_string_lossy().replace('\\', "/")
                        );
                        prop_assert!(
                            !calls.contains(&listing),
                            "a mount root was listed under Stay: {listing}\n{}",
                            fixture_snippet(&case)
                        );
                    }
                }
            }
        }
    }
}
