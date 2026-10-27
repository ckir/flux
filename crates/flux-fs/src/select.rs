// Copyright (c) 2026 Costas Kirgoussios
//
// This software is licensed under the PolyForm Noncommercial License 1.0.0.
// You may obtain a copy of the License at:
// https://polyformproject.org/licenses/noncommercial/1.0.0/

//! The `--exclude` matcher and the `--recursive` decision (design 5.1, 5.2).
//!
//! A [`Pattern`] is parsed once from its typed form. [`Selection::select`] reads
//! `rel.components()` and never splits path bytes, so a backslash inside a Unix
//! name is an ordinary character. Wildcard matching is the iterative two-pointer
//! form (backtrack only to the last star), so one match costs
//! `components x segments x glob length` and the tests assert it with a counter.

use std::ffi::OsStr;
use std::path::{Component, Path};

use crate::FileType;

/// Longest accepted pattern, in UTF-8 bytes.
pub const MAX_PATTERN_BYTES: usize = 1024;
/// Most `/`-separated segments one pattern may have.
pub const MAX_PATTERN_SEGMENTS: usize = 64;
/// Most patterns accepted across the command line, counted as given.
pub const MAX_PATTERNS: usize = 64;
/// Most pattern bytes accepted across the command line, counted as given.
pub const MAX_TOTAL_PATTERN_BYTES: usize = 16 * 1024;

/// Why a pattern list was refused. Every message names the pattern and the reason.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SelectError {
    #[error("pattern is empty")]
    Empty,
    #[error("pattern {0:?} is only `/`")]
    OnlySlash(String),
    #[error("pattern {0:?} has an empty segment")]
    EmptySegment(String),
    #[error("pattern {0:?} has a `.` or `..` segment")]
    DotSegment(String),
    #[error("pattern {0:?} contains a backslash: use `/` as the separator")]
    Backslash(String),
    #[error("pattern {pattern:?} contains the reserved or control character {ch:?}")]
    BadChar { pattern: String, ch: char },
    #[error("pattern {0:?} uses `**` inside a segment; `**` must be a whole segment")]
    MisplacedDoubleStar(String),
    #[error("pattern {0:?} has two adjacent `**` segments")]
    AdjacentDoubleStar(String),
    #[error("pattern {pattern:?} is {len} bytes, more than {MAX_PATTERN_BYTES}")]
    TooLong { pattern: String, len: usize },
    #[error("pattern {pattern:?} has {count} segments, more than {MAX_PATTERN_SEGMENTS}")]
    TooManySegments { pattern: String, count: usize },
    #[error("{count} patterns given, more than {MAX_PATTERNS}")]
    TooManyPatterns { count: usize },
    #[error("{bytes} bytes of patterns given, more than {MAX_TOTAL_PATTERN_BYTES}")]
    TooManyBytes { bytes: usize },
}

/// One pattern element inside a component. `Lit` holds a Unicode scalar in scalar
/// mode and one byte in byte mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tok {
    Lit(u32),
    Any,
    Star,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Seg {
    /// Zero or more whole components.
    Deep,
    /// Exactly one component, whatever it is (the "one or more" of a trailing `**`).
    One,
    /// One component matched by a glob, held in both encodings.
    Glob { scalars: Vec<Tok>, bytes: Vec<Tok> },
}

/// A parsed `--exclude` pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pattern {
    raw: String,
    dir_only: bool,
    anchored: bool,
    segs: Vec<Seg>,
}

impl Pattern {
    /// Parses and validates one pattern.
    ///
    /// # Errors
    /// [`SelectError`] naming the pattern and the reason.
    pub fn parse(s: &str) -> Result<Pattern, SelectError> {
        let raw = s.to_string();
        if s.is_empty() {
            return Err(SelectError::Empty);
        }
        if s.len() > MAX_PATTERN_BYTES {
            return Err(SelectError::TooLong { pattern: raw, len: s.len() });
        }
        for ch in s.chars() {
            if ch == '\\' {
                return Err(SelectError::Backslash(raw));
            }
            if matches!(ch, '!' | '[' | ']') || ch.is_control() {
                return Err(SelectError::BadChar { pattern: raw, ch });
            }
        }
        if s == "/" {
            return Err(SelectError::OnlySlash(raw));
        }
        let (body, dir_only) = match s.strip_suffix('/') {
            Some(b) => (b, true),
            None => (s, false),
        };
        let (body, lead) = match body.strip_prefix('/') {
            Some(b) => (b, true),
            None => (body, false),
        };
        let parts: Vec<&str> = body.split('/').collect();
        if parts.len() > MAX_PATTERN_SEGMENTS {
            return Err(SelectError::TooManySegments { pattern: raw, count: parts.len() });
        }
        let anchored = lead || parts.len() > 1;
        let mut segs = Vec::with_capacity(parts.len() + 1);
        let mut prev_deep = false;
        for (i, part) in parts.iter().enumerate() {
            if part.is_empty() {
                return Err(SelectError::EmptySegment(raw));
            }
            if *part == "." || *part == ".." {
                return Err(SelectError::DotSegment(raw));
            }
            if *part == "**" {
                if prev_deep {
                    return Err(SelectError::AdjacentDoubleStar(raw));
                }
                prev_deep = true;
                if i + 1 == parts.len() {
                    // A trailing `**` is one or more components.
                    segs.push(Seg::One);
                }
                segs.push(Seg::Deep);
                continue;
            }
            prev_deep = false;
            if part.contains("**") {
                return Err(SelectError::MisplacedDoubleStar(raw));
            }
            segs.push(glob(part));
        }
        Ok(Pattern { raw, dir_only, anchored, segs })
    }

    /// The pattern as typed.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.raw
    }

    fn matches<C: Counter>(&self, comps: &[Comp], ft: FileType, c: &mut C) -> bool {
        if self.dir_only && ft != FileType::Dir {
            return false;
        }
        let cand = if self.anchored {
            comps
        } else {
            match comps.last() {
                Some(_) => &comps[comps.len() - 1..],
                None => return false,
            }
        };
        match_segs(&self.segs, cand, c)
    }
}

fn glob(part: &str) -> Seg {
    let mut scalars = Vec::new();
    let mut bytes = Vec::new();
    for ch in part.chars() {
        match ch {
            '*' => {
                scalars.push(Tok::Star);
                bytes.push(Tok::Star);
            }
            '?' => {
                scalars.push(Tok::Any);
                bytes.push(Tok::Any);
            }
            _ => {
                scalars.push(Tok::Lit(u32::from(ch)));
                let mut buf = [0u8; 4];
                for b in ch.encode_utf8(&mut buf).bytes() {
                    bytes.push(Tok::Lit(u32::from(b)));
                }
            }
        }
    }
    Seg::Glob { scalars, bytes }
}

/// Counts matcher steps. The unit impl is a no-op, so the production path pays
/// nothing and `Selection` holds no interior state.
trait Counter {
    fn tick(&mut self);
}

struct NoCount;

impl Counter for NoCount {
    #[inline]
    fn tick(&mut self) {}
}

#[cfg(test)]
impl Counter for u64 {
    fn tick(&mut self) {
        *self += 1;
    }
}

/// One path component as matchable units: scalars when the name is valid UTF-8,
/// else bytes of its encoded form. Decided once per component.
struct Comp {
    is_scalars: bool,
    units: Vec<u32>,
}

impl Comp {
    fn new(name: &OsStr) -> Comp {
        match name.to_str() {
            Some(s) => Comp { is_scalars: true, units: s.chars().map(u32::from).collect() },
            None => Comp {
                is_scalars: false,
                units: name.as_encoded_bytes().iter().map(|b| u32::from(*b)).collect(),
            },
        }
    }
}

/// Iterative two-pointer glob match of one component.
fn match_glob<C: Counter>(toks: &[Tok], units: &[u32], c: &mut C) -> bool {
    let (mut p, mut t) = (0usize, 0usize);
    let mut star: Option<(usize, usize)> = None;
    while t < units.len() {
        c.tick();
        if let Some(tok) = toks.get(p) {
            match *tok {
                Tok::Star => {
                    star = Some((p, t));
                    p += 1;
                    continue;
                }
                Tok::Any => {
                    p += 1;
                    t += 1;
                    continue;
                }
                Tok::Lit(x) if x == units[t] => {
                    p += 1;
                    t += 1;
                    continue;
                }
                Tok::Lit(_) => {}
            }
        }
        match star {
            Some((sp, st)) => {
                p = sp + 1;
                t = st + 1;
                star = Some((sp, st + 1));
            }
            None => return false,
        }
    }
    while toks.get(p) == Some(&Tok::Star) {
        c.tick();
        p += 1;
    }
    p == toks.len()
}

fn match_seg<C: Counter>(seg: &Seg, comp: &Comp, c: &mut C) -> bool {
    match seg {
        Seg::One => true,
        Seg::Deep => unreachable!("a deep segment is handled by the caller"),
        Seg::Glob { scalars, bytes } => {
            let toks = if comp.is_scalars { scalars } else { bytes };
            match_glob(toks, &comp.units, c)
        }
    }
}

/// The same two-pointer form over whole components, `**` being the star.
fn match_segs<C: Counter>(segs: &[Seg], comps: &[Comp], c: &mut C) -> bool {
    let (mut p, mut t) = (0usize, 0usize);
    let mut star: Option<(usize, usize)> = None;
    while t < comps.len() {
        c.tick();
        if let Some(seg) = segs.get(p) {
            if *seg == Seg::Deep {
                star = Some((p, t));
                p += 1;
                continue;
            }
            if match_seg(seg, &comps[t], c) {
                p += 1;
                t += 1;
                continue;
            }
        }
        match star {
            Some((sp, st)) => {
                p = sp + 1;
                t = st + 1;
                star = Some((sp, st + 1));
            }
            None => return false,
        }
    }
    while segs.get(p) == Some(&Seg::Deep) {
        c.tick();
        p += 1;
    }
    p == segs.len()
}

/// What to do with one entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Copy it, and enter it if it is a directory.
    Keep,
    /// A pattern matched; it is never entered.
    Excluded,
    /// A directory under `--no-recursive`; not entered, not an exclusion.
    Skipped,
}

/// The compiled selection: the `--exclude` patterns and the recursion flag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    typed: Vec<String>,
    parsed: Vec<Pattern>,
    recursive: bool,
}

impl Default for Selection {
    fn default() -> Self {
        Selection { typed: Vec::new(), parsed: Vec::new(), recursive: true }
    }
}

impl Selection {
    /// Validates the list as given (count and total bytes before deduplication),
    /// then sorts and deduplicates the typed strings.
    ///
    /// # Errors
    /// [`SelectError`] for an invalid pattern or an exceeded aggregate cap.
    pub fn compile(patterns: &[String], recursive: bool) -> Result<Selection, SelectError> {
        if patterns.len() > MAX_PATTERNS {
            return Err(SelectError::TooManyPatterns { count: patterns.len() });
        }
        let bytes: usize = patterns.iter().map(String::len).sum();
        if bytes > MAX_TOTAL_PATTERN_BYTES {
            return Err(SelectError::TooManyBytes { bytes });
        }
        let mut typed: Vec<String> = patterns.to_vec();
        typed.sort();
        typed.dedup();
        let parsed = typed.iter().map(|s| Pattern::parse(s)).collect::<Result<Vec<_>, _>>()?;
        Ok(Selection { typed, parsed, recursive })
    }

    /// The patterns as typed, sorted and deduplicated.
    #[must_use]
    pub fn patterns(&self) -> &[String] {
        &self.typed
    }

    /// Whether directories are entered.
    #[must_use]
    pub fn recursive(&self) -> bool {
        self.recursive
    }

    /// True for no patterns and recursive.
    #[must_use]
    pub fn is_default(&self) -> bool {
        self.typed.is_empty() && self.recursive
    }

    /// Decides one entry by its path relative to the source root.
    #[must_use]
    pub fn select(&self, rel: &Path, ft: FileType) -> Verdict {
        self.select_with(rel, ft, &mut NoCount)
    }

    #[cfg(test)]
    pub(crate) fn select_counted(&self, rel: &Path, ft: FileType, steps: &mut u64) -> Verdict {
        self.select_with(rel, ft, steps)
    }

    fn select_with<C: Counter>(&self, rel: &Path, ft: FileType, c: &mut C) -> Verdict {
        if !self.parsed.is_empty() {
            let comps: Vec<Comp> = rel
                .components()
                .filter_map(|k| match k {
                    Component::Normal(n) => Some(Comp::new(n)),
                    _ => None,
                })
                .collect();
            if self.parsed.iter().any(|p| p.matches(&comps, ft, c)) {
                return Verdict::Excluded;
            }
        }
        if !self.recursive && ft == FileType::Dir {
            return Verdict::Skipped;
        }
        Verdict::Keep
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn rel(parts: &[&str]) -> PathBuf {
        let mut p = PathBuf::new();
        for part in parts {
            p = p.join(part);
        }
        p
    }

    fn sel(pats: &[&str], recursive: bool) -> Selection {
        let v: Vec<String> = pats.iter().map(|s| (*s).to_string()).collect();
        Selection::compile(&v, recursive).expect("valid patterns")
    }

    fn verdict(pat: &str, parts: &[&str], ft: FileType) -> Verdict {
        sel(&[pat], true).select(&rel(parts), ft)
    }

    #[test]
    fn selection_is_sync() {
        fn assert_sync<T: Sync + Send>() {}
        assert_sync::<Selection>();
    }

    #[test]
    fn the_design_table_is_decided_exactly() {
        use FileType::{Dir, File};
        use Verdict::{Excluded, Keep};
        let table: &[(&str, &[&str], FileType, Verdict)] = &[
            ("private/**", &["private", "file"], File, Excluded),
            ("private/**", &["private"], Dir, Keep),
            ("private/**", &["private", "a", "b"], File, Excluded),
            ("private", &["private"], Dir, Excluded),
            ("private/", &["private"], Dir, Excluded),
            ("private/", &["private"], File, Keep),
            ("*.tmp", &["cache.tmp"], File, Excluded),
            ("*.tmp", &["sub", "deep", "cache.tmp"], File, Excluded),
            ("*.tmp", &["x.tmp", "f"], File, Keep),
            ("sub/*.tmp", &["sub", "x.tmp"], File, Excluded),
            ("sub/*.tmp", &["other", "sub", "x.tmp"], File, Keep),
            ("sub/*.tmp", &["sub", "x.tmp", "y"], File, Keep),
            ("/foo", &["foo"], File, Excluded),
            ("/foo", &["a", "foo"], File, Keep),
            ("foo", &["a", "b", "foo"], File, Excluded),
            ("docs/**/notes.txt", &["docs", "notes.txt"], File, Excluded),
            ("docs/**/notes.txt", &["docs", "2026", "10", "notes.txt"], File, Excluded),
            ("build/*", &["build", "x", "y"], File, Keep),
            ("**", &["a"], File, Excluded),
            ("**", &["a", "b", "c"], Dir, Excluded),
            ("/**", &["a"], File, Excluded),
            ("/**", &["a", "b"], Dir, Excluded),
            ("**/", &["d"], Dir, Excluded),
            ("**/", &["f"], File, Keep),
            ("**/foo", &["foo"], File, Excluded),
            ("**/foo", &["a", "foo"], File, Excluded),
            ("foo/", &["a", "b", "foo"], Dir, Excluded),
            ("foo/", &["a", "b", "foo"], File, Keep),
            ("*", &[".hidden"], File, Excluded),
            ("README", &["readme"], File, Keep),
            ("\u{c9}", &["\u{e9}"], File, Keep),
            ("fil?", &["fil\u{e9}"], File, Excluded),
            ("fil??", &["fil\u{e9}"], File, Keep),
        ];
        for (pat, parts, ft, want) in table {
            assert_eq!(verdict(pat, parts, *ft), *want, "pattern {pat:?} vs {parts:?} {ft:?}");
        }
    }

    #[test]
    fn question_mark_counts_scalars_for_valid_names_and_bytes_otherwise() {
        use FileType::File;
        assert_eq!(verdict("?", &["\u{20ac}"], File), Verdict::Excluded);
        assert_eq!(verdict("?", &["\u{1f600}"], File), Verdict::Excluded);
        assert_eq!(verdict("??", &["\u{20ac}"], File), Verdict::Keep);
        #[cfg(unix)]
        {
            use std::ffi::OsStr;
            use std::os::unix::ffi::OsStrExt;
            let bad = rel(&[]).join(OsStr::from_bytes(b"fil\xff"));
            assert_eq!(sel(&["fil?"], true).select(&bad, File), Verdict::Excluded);
            let pair = PathBuf::from(OsStr::from_bytes(b"\xe2\x82"));
            assert_eq!(sel(&["??"], true).select(&pair, File), Verdict::Excluded);
            let mixed = PathBuf::from("\u{e9}").join(OsStr::from_bytes(b"fil\xff"));
            assert_eq!(sel(&["?/fil?"], true).select(&mixed, File), Verdict::Excluded);
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_backslash_in_a_name_is_not_a_separator() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        let p = PathBuf::from(OsStr::from_bytes(b"a\\b"));
        assert_eq!(sel(&["a?b"], true).select(&p, FileType::File), Verdict::Excluded);
        assert_eq!(sel(&["a/b"], true).select(&p, FileType::File), Verdict::Keep);
    }

    #[test]
    fn recursion_skips_directories_after_patterns() {
        let none = sel(&[], false);
        assert_eq!(none.select(&rel(&["d"]), FileType::Dir), Verdict::Skipped);
        for ft in [FileType::File, FileType::Symlink, FileType::Other] {
            assert_eq!(none.select(&rel(&["d"]), ft), Verdict::Keep);
        }
        let with = sel(&["build/"], false);
        assert_eq!(with.select(&rel(&["build"]), FileType::Dir), Verdict::Excluded);
        assert_eq!(with.select(&rel(&["other"]), FileType::Dir), Verdict::Skipped);
        assert_eq!(with.select(&rel(&["build"]), FileType::Symlink), Verdict::Keep);
    }

    #[test]
    fn invalid_patterns_are_refused_with_a_reason() {
        for bad in [
            "", "/", "a//b", "..", "a/../b", "./a", ".", "a!b", "[a]", "a]b", "a**", "**a", "***",
            "a*b**", "**/**", "a\nb", "a\tb", "a\u{85}b", "a\0b", "a\u{7f}b",
        ] {
            assert!(Pattern::parse(bad).is_err(), "{bad:?} must be refused");
        }
        let msg = Pattern::parse("a\\b").unwrap_err().to_string();
        assert!(msg.contains("use `/`"), "{msg}");
        assert!(msg.contains(r#""a\\b""#), "names the pattern: {msg}");
        assert!(Pattern::parse(&"a".repeat(1025)).is_err());
        assert!(Pattern::parse(&"a".repeat(1024)).is_ok());
        let segs = |n: usize| vec!["a"; n].join("/");
        assert!(Pattern::parse(&segs(65)).is_err());
        assert!(Pattern::parse(&segs(64)).is_ok());
        assert_eq!(Pattern::parse("a/b/").unwrap().as_str(), "a/b/");
    }

    fn strings(v: impl IntoIterator<Item = String>) -> Vec<String> {
        v.into_iter().collect()
    }

    fn big(i: usize) -> String {
        format!("{}{i:04}", "x".repeat(1020))
    }

    #[test]
    fn the_aggregate_caps_apply() {
        let distinct = |n: usize| strings((0..n).map(|i| format!("p{i}")));
        assert!(Selection::compile(&distinct(65), true).is_err());
        assert!(Selection::compile(&distinct(64), true).is_ok());
        assert!(Selection::compile(&vec!["same".to_string(); 65], true).is_err());
        assert!(Selection::compile(&strings((0..17).map(big)), true).is_err());
        assert!(Selection::compile(&strings((0..16).map(big)), true).is_ok());
        let mut v = strings((0..16).map(big));
        assert!(Selection::compile(&v, true).is_ok());
        v.push("y".to_string());
        assert!(Selection::compile(&v, true).is_err());
    }

    #[test]
    fn compile_sorts_and_deduplicates() {
        let a = Selection::compile(&["b".into(), "a".into(), "a".into()], true).unwrap();
        let b = Selection::compile(&["a".into(), "b".into()], true).unwrap();
        assert_eq!(a, b);
        assert_eq!(a.patterns(), ["a", "b"]);
        assert!(a.recursive());
        assert!(Selection::default().is_default());
        assert!(Selection::default().patterns().is_empty());
        assert!(Selection::default().recursive());
        assert!(!Selection::compile(&[], false).unwrap().is_default());
        assert!(!a.is_default());
    }

    #[test]
    fn a_pathological_pattern_stays_within_a_step_bound() {
        let mut pat = "a/**/".repeat(31);
        pat.push_str("a/b");
        assert_eq!(pat.split('/').count(), 64);
        let s = sel(&[&pat], true);
        let parts = vec!["a"; 200];
        let mut steps = 0u64;
        let v = s.select_counted(&rel(&parts), FileType::File, &mut steps);
        assert_eq!(v, Verdict::Keep);
        assert!(steps <= 200 * 64 * 8, "steps {steps}");

        let s = sel(&["a*a*a*a*a*a*a*a*a*b"], true);
        let name = "a".repeat(2000);
        let mut steps = 0u64;
        let v = s.select_counted(&rel(&[&name]), FileType::File, &mut steps);
        assert_eq!(v, Verdict::Keep);
        assert!(steps <= 2000 * 40, "steps {steps}");
    }

    #[test]
    fn one_entry_two_patterns_is_one_verdict() {
        assert_eq!(
            sel(&["*.tmp", "cache*"], true).select(&rel(&["cache.tmp"]), FileType::File),
            Verdict::Excluded
        );
    }
}
