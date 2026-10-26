// Copyright (c) 2026 Costas Kirgoussios
//
// This software is licensed under the PolyForm Noncommercial License 1.0.0.
// You may obtain a copy of the License at:
// https://polyformproject.org/licenses/noncommercial/1.0.0/

//! The verify-merge property test's case model: the generated `Case`, and the pure
//! derivation of the two trees (source and destination) it describes. Later pieces
//! (materialisation, the reference, the generator and the property) build on it.

// Transitional: the generator, reference and property that read every variant and field land in later tasks.
#![allow(dead_code)]

use crate::walk::MountPolicy;
use flux_fs::{Code, Links, MountRoot};
use std::collections::BTreeMap;
use std::path::PathBuf;

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
}
