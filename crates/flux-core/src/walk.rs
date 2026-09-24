//! The ordered directory walk (§7.2, §9, §149.4).

use flux_fs::{Code, DirEntry, FileIdentity, FileSystem, FileType, FsError, ObjectId, Result};
use std::path::{Path, PathBuf};

/// Chosen to sit BELOW the `PATH_MAX`-implied bound of roughly 370 levels rather
/// than above it, so this guard is the operative one both now and after any move to
/// relative traversal -- the behaviour does not change out from under a user when
/// the traversal does. MEASURED on WSL Ubuntu-26.04: nested directories built with
/// relative paths and `chdir` reached 5000 levels at a path length of 55,020 bytes,
/// over thirteen times `getconf PATH_MAX`, so the kernel enforces no cumulative
/// depth limit of its own. Real trees are an order of magnitude shallower than 256.
pub const DEFAULT_MAX_DEPTH: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WalkEvent {
    /// Pre-order: the directory exists and is about to be descended into.
    Dir {
        path: PathBuf,
        identity: FileIdentity,
    },
    File {
        path: PathBuf,
    },
    Symlink {
        path: PathBuf,
    },
    Other {
        path: PathBuf,
    },
    /// Post-order: every descendant of this directory has been yielded.
    ///
    /// Not speculative generality. §30.2 requires that directory metadata sensitive
    /// to child mutations is not finalized before the children are written, so a
    /// directory is created pre-order and finalized post-order. A pre-order-only
    /// stream cannot express "leaving this directory", so a consumer would have to
    /// re-derive it by comparing path prefixes -- fragile exactly where §7.2's
    /// component-wise ordering is subtle.
    ///
    /// THIS walk emits it. PR 3 does NOT consume it, because directory metadata is
    /// not preserved in this cut -- `set_times` takes a `&Self::Writer`, a file
    /// handle, and widening that surface is a separate decision. It is emitted now
    /// precisely so the walker does not change when directory metadata arrives.
    DirEnd {
        path: PathBuf,
    },
}

/// One failed entry. NOT terminal: the walk reports it, skips that subtree, and
/// continues with the next sibling.
#[derive(Debug)]
pub struct WalkError {
    /// Relative to the walk root, like every path in a `WalkEvent`.
    pub path: PathBuf,
    pub cause: FsError,
}

/// One directory being enumerated.
struct Frame {
    /// Relative to the walk root. Empty for the root frame.
    rel: PathBuf,
    entries: std::vec::IntoIter<DirEntry>,
    /// Whether this frame pushed onto `ancestors`, so the pop stays balanced when
    /// the identity was not `Strong` and nothing was pushed.
    pushed_ancestor: bool,
    /// False for the root frame, which emits no event of any kind.
    emit_end: bool,
}

/// Borrows the filesystem for the life of the walk; holds the explicit stack, the
/// ancestor set, and the root it makes paths relative to.
pub struct Walk<'a, F: FileSystem> {
    fs: &'a F,
    root: PathBuf,
    max_depth: usize,
    stack: Vec<Frame>,
    /// The `ObjectId`s of the directories on the CURRENT path -- not a global
    /// visited set, which would grow with the total number of directories and is
    /// forbidden by spec line 997. An ancestor set is bounded by depth and is
    /// sufficient, because a cycle by definition re-enters an ancestor.
    ///
    /// Only `Strong` identities are ever pushed, so "both sides Strong" holds by
    /// construction rather than by a second check at comparison time.
    ancestors: Vec<ObjectId>,
}

// Manual, not derived: `#[derive(Debug)]` on a generic struct adds a `F: Debug`
// bound, and the fakes this walk is tested against (e.g. `FaultFs`) do not
// implement it. `Result::unwrap_err` requires the `Ok` side to be `Debug`, which
// `walk()`'s tests need, so this exists to satisfy that without constraining `F`.
impl<'a, F: FileSystem> std::fmt::Debug for Walk<'a, F> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Walk")
            .field("root", &self.root)
            .field("max_depth", &self.max_depth)
            .field("stack_depth", &self.stack.len())
            .field("ancestors_depth", &self.ancestors.len())
            .finish()
    }
}

/// Sort one directory by `name.as_encoded_bytes()`, ascending. Nothing else.
///
/// §7.2: "A depth-first scanner that sorts each directory's entries by name alone
/// emits component-wise order without buffering." Depth-first plus this gives
/// `a/x  a/y/z  a-b  a0`; a flat `/`-joined sort gives `a-b  a/x  a/y/z  a0`, which
/// §7.2 calls wrong.
///
/// `as_encoded_bytes()` is exactly the §241 representation on both platforms, with
/// no conversion and no allocation. `OsStrExt::encode_wide()` must NEVER be used
/// here -- see the test in `flux-fs`.
fn sorted(mut v: Vec<DirEntry>) -> Vec<DirEntry> {
    v.sort_by(|a, b| a.name.as_encoded_bytes().cmp(b.name.as_encoded_bytes()));
    v
}

/// A `DirEntry` name must be exactly ONE ordinary component.
///
/// `PathBuf::join` DISCARDS the base when the appended component is absolute --
/// MEASURED: `Path::new("sub").join("/etc/shadow")` is `"/etc/shadow"`, and with
/// `C:\Windows` it is `C:\Windows`. So an entry name that is not a bare component
/// would walk the iterator straight out of its own root, which is the same class of
/// escape `Metadata::file_type` exists to prevent.
///
/// `..` and `.` stay relative under `join` and so cannot escape by that route, but
/// they are refused too: neither is an entry a directory listing may contain.
fn is_one_component(name: &std::ffi::OsStr) -> bool {
    let mut c = Path::new(name).components();
    matches!(c.next(), Some(std::path::Component::Normal(_))) && c.next().is_none()
}

/// `root` must name a directory. The walk yields NO event for the root itself --
/// its first event is the root's first child -- because the root is not part of the
/// tree being copied INTO the destination, it IS the destination mapping.
pub fn walk<'a, F: FileSystem>(fs: &'a F, root: &Path) -> Result<Walk<'a, F>> {
    walk_with_depth(fs, root, DEFAULT_MAX_DEPTH)
}

/// As `walk`, with the depth cap chosen explicitly.
pub fn walk_with_depth<'a, F: FileSystem>(
    fs: &'a F,
    root: &Path,
    max_depth: usize,
) -> Result<Walk<'a, F>> {
    // Fallible BEFORE anything is yielded: a missing root, or a root that is not a
    // directory, is a failure of the whole operation rather than a per-entry error,
    // so it is reported through `Result` and not as a first `Err` item.
    let m = fs.metadata(root)?;
    if m.file_type != FileType::Dir {
        return Err(FsError::new(
            Code::SpecialFileUnsupported,
            std::io::Error::other("walk root is not a directory"),
        ));
    }
    let entries = sorted(fs.read_dir(root)?);

    // The root goes into the ancestor set even though no event is emitted for it.
    // Load-bearing, not tidy: the measured bind-mount case is `mount --bind a a/b`,
    // where the re-entered directory IS the root, so a walk that only tracked
    // directories it emitted events for would copy the whole tree twice.
    let mut ancestors = Vec::new();
    let pushed_ancestor = match m.identity {
        FileIdentity::Strong(id) => {
            ancestors.push(id);
            true
        }
        _ => false,
    };

    Ok(Walk {
        fs,
        root: root.to_path_buf(),
        max_depth,
        stack: vec![Frame {
            rel: PathBuf::new(),
            entries: entries.into_iter(),
            pushed_ancestor,
            emit_end: false,
        }],
        ancestors,
    })
}

/// **An `Err` item is NOT terminal.** The iterator yields the error, skips that
/// subtree, and continues with the next sibling. Most fallible Rust iterators stop,
/// which is exactly why this is stated here.
///
/// Spec item 83, verbatim: "`DIRECTORY_CHANGED_DURING_SCAN` has one outcome: the
/// directory's subtree is not transferred, the error is reported, and the operation
/// exits 1. There is no configured mutation policy or rescan alternative." The
/// operation's exit status is 1 even though the walk COMPLETED.
///
/// The spec mandates this shape only for identity changes; applying it to every
/// per-directory failure is a design decision, taken because the alternative is
/// aborting a whole tree copy over one denied subdirectory.
///
/// A pull iterator, because §9 requires that when downstream capacity is exhausted
/// the "scanner blocks/awaits" rather than accumulating paths. A consumer that stops
/// pulling IS the backpressure, so Phase 3's bounded queue sits between this
/// iterator and the workers without the walker changing.
impl<'a, F: FileSystem> Iterator for Walk<'a, F> {
    type Item = std::result::Result<WalkEvent, WalkError>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if self.stack.is_empty() {
                return None;
            }
            // Take the next entry and release the borrow before `enter` needs
            // `&mut self`. `join` allocates the path we were going to need anyway.
            let step = {
                let frame = self.stack.last_mut().expect("stack checked non-empty");
                frame.entries.next().map(|e| {
                    // PANIC, not an `Err` item. A real filesystem CANNOT produce this:
                    // neither Linux nor Windows permits a separator inside a file name,
                    // so the only way here is a `FileSystem` implementor breaking the
                    // contract `read_dir` documents -- a programmer bug, which is what
                    // `panic!` is for, against `Result` for an environment fault.
                    //
                    // An `Err` item was the obvious choice and it is wrong: every other
                    // error here is non-terminal, so a broken adapter would silently
                    // OMIT entries from a copy while every test stayed green. That is
                    // the same trap `FaultFs::metadata` already refuses, panicking
                    // rather than reporting `Unavailable` because `Unavailable` is a
                    // state the walk handles gracefully.
                    //
                    // Unreachable by construction rather than merely unlikely: both
                    // in-tree implementors build the name from `file_name()`.
                    assert!(
                        is_one_component(&e.name),
                        "FileSystem::read_dir returned an entry name that is not a \
                         single component: {:?} in {:?}. A name must be one bare \
                         component; `join` discards the base on an absolute one and \
                         the walk would leave its root.",
                        e.name,
                        frame.rel
                    );
                    (frame.rel.join(&e.name), e.file_type)
                })
            };

            match step {
                Some((path, FileType::File)) => return Some(Ok(WalkEvent::File { path })),
                Some((path, FileType::Symlink)) => return Some(Ok(WalkEvent::Symlink { path })),
                Some((path, FileType::Other)) => return Some(Ok(WalkEvent::Other { path })),
                Some((path, FileType::Dir)) => return Some(self.enter(path)),
                None => {
                    // This directory is exhausted. Unwind it, keeping the ancestor
                    // set balanced, and close it post-order.
                    let f = self.stack.pop().expect("stack checked non-empty");
                    if f.pushed_ancestor {
                        self.ancestors.pop();
                    }
                    if f.emit_end {
                        return Some(Ok(WalkEvent::DirEnd { path: f.rel }));
                    }
                    // The root frame emits nothing; the loop ends on the next pass.
                }
            }
        }
    }
}

impl<'a, F: FileSystem> Walk<'a, F> {
    /// Descend into `rel`, or refuse it. On `Ok` a frame has been pushed and the
    /// pre-order `Dir` event is returned; on `Err` nothing was pushed, the subtree
    /// is skipped, and the walk continues with the next sibling.
    fn enter(&mut self, rel: PathBuf) -> std::result::Result<WalkEvent, WalkError> {
        // The cap runs at EVERY identity strength, and is the only guard that does.
        // `stack.len()` counts frames, and the root frame is one, so a child of the
        // root is depth 1.
        //
        // It lands HERE, with the iterator, rather than in its own later task, and
        // that is forced for the same reason Tasks 7 and 8 are one commit: `Walk`
        // stores `max_depth`, and a field that is written but never READ is
        // `dead_code`, which is deny-level under `clippy -D warnings`. Task 10 adds
        // the TESTS that pin this check; the check itself cannot wait for them.
        if self.stack.len() > self.max_depth {
            return Err(WalkError {
                path: rel,
                cause: FsError::new(
                    Code::IoError,
                    // No OS error exists for "too deep", and no spec code names it.
                    // `IoError` plus a message, rather than misusing a spec code:
                    // `SafetyRejected` aborts the whole operation and this does not.
                    std::io::Error::other("directory depth cap exceeded"),
                ),
            });
        }

        let abs = self.root.join(&rel);

        let m = match self.fs.metadata(&abs) {
            Ok(m) => m,
            Err(cause) => return Err(WalkError { path: rel, cause }),
        };

        // §149.4. `read_dir` typed this entry `Dir`; if `metadata` disagrees, the
        // object under the name changed between the listing and the stat.
        //
        // This NARROWS the race, it does not close it: the window moves from
        // read_dir-to-metadata down to metadata-to-read_dir, and something could
        // still be swapped inside it. Closing it needs `openat`-style relative
        // traversal with `O_NOFOLLOW`, which `TODO.md` already records as the fix
        // for walker TOCTOU. Do not let this comment claim more than that.
        if m.file_type != FileType::Dir {
            return Err(WalkError {
                path: rel,
                cause: FsError::new(
                    Code::DirectoryChangedDuringScan,
                    std::io::Error::other("entry listed as a directory is no longer one"),
                ),
            });
        }

        // Only `Strong` is ever pushed OR compared, so "both sides Strong" holds by
        // construction. §107: a Weak identity exists and cannot be trusted, and a
        // comparison is no stronger than its weaker operand. Asymmetry is the normal
        // case -- a local ext4 source against an SMB destination -- so this is the
        // common path, not a corner.
        let mut pushed_ancestor = false;
        if let FileIdentity::Strong(id) = m.identity {
            if self.ancestors.contains(&id) {
                return Err(WalkError {
                    path: rel,
                    cause: FsError::new(
                        Code::IoError,
                        // Not `SafetyRejected`: that aborts the whole operation
                        // under §129 and a cycle explicitly does not, so the same
                        // code would carry two different severities.
                        // `ErrorKind::FilesystemLoop` would be the precise kind but
                        // is unstable on the pinned toolchain (E0658, issue #86442).
                        std::io::Error::other("directory cycle: already an ancestor"),
                    ),
                });
            }
            self.ancestors.push(id);
            pushed_ancestor = true;
        }

        let entries = match self.fs.read_dir(&abs) {
            Ok(v) => sorted(v),
            Err(cause) => {
                // Balance the set: this frame is never pushed, so nothing will pop.
                if pushed_ancestor {
                    self.ancestors.pop();
                }
                return Err(WalkError { path: rel, cause });
            }
        };

        self.stack.push(Frame {
            rel: rel.clone(),
            entries: entries.into_iter(),
            pushed_ancestor,
            emit_end: true,
        });
        Ok(WalkEvent::Dir { path: rel, identity: m.identity })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fault_fs::FaultFs;

    #[test]
    fn walk_refuses_a_root_that_is_not_a_directory() {
        let fs = FaultFs::new();
        fs.write_file("/f", b"x");
        let e = walk(&fs, Path::new("/f")).unwrap_err();
        assert_eq!(e.code, Code::SpecialFileUnsupported);
    }

    #[test]
    fn walk_refuses_a_root_that_does_not_exist() {
        let fs = FaultFs::new();
        assert!(walk(&fs, Path::new("/nope")).is_err());
    }

    #[test]
    fn an_empty_root_yields_nothing() {
        // No event for the root itself: the root IS the destination mapping, not
        // part of the tree being copied into it.
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/r")).unwrap();
        let got: Vec<_> = walk(&fs, Path::new("/r")).unwrap().collect();
        assert!(got.is_empty(), "got {got:?}");
    }

    /// `/r/a/x`, `/r/a/y/z`, `/r/a-b`, `/r/a0` -- the design document's own
    /// normative §7.2 example, which distinguishes component-wise order from a flat
    /// `/`-joined sort.
    fn component_order_tree() -> FaultFs {
        let fs = FaultFs::new();
        for d in ["/r", "/r/a", "/r/a/y"] {
            fs.create_dir(Path::new(d)).unwrap();
        }
        fs.write_file("/r/a/x", b"");
        fs.write_file("/r/a/y/z", b"");
        fs.write_file("/r/a-b", b"");
        fs.write_file("/r/a0", b"");
        fs
    }

    /// Render a relative path with `/` on EVERY platform.
    ///
    /// `Display` must NOT be used for this. MEASURED on Windows:
    /// `PathBuf::new().join("a").join("y").join("z").display().to_string()` is
    /// `"a\\y\\z"`, so an assertion written against `"a/y/z"` fails on the primary
    /// dev platform while passing on Linux. Joining COMPONENTS sidesteps the
    /// separator entirely and was measured to give `"a/y/z"` on both.
    ///
    /// This is only about the ASSERTION's spelling. `Path` itself compares, hashes
    /// and takes `parent()` separator-insensitively on Windows -- also measured --
    /// so the fake's `PathBuf` keys are fine either way.
    fn rel(p: &Path) -> String {
        p.components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/")
    }

    fn paths(fs: &FaultFs) -> Vec<String> {
        walk(fs, Path::new("/r"))
            .unwrap()
            .map(|r| match r.unwrap() {
                WalkEvent::Dir { path, .. } => format!("D {}", rel(&path)),
                WalkEvent::File { path } => format!("F {}", rel(&path)),
                WalkEvent::Symlink { path } => format!("L {}", rel(&path)),
                WalkEvent::Other { path } => format!("O {}", rel(&path)),
                WalkEvent::DirEnd { path } => format!("E {}", rel(&path)),
            })
            .collect()
    }

    #[test]
    fn it_emits_component_wise_order() {
        let got = paths(&component_order_tree());
        assert_eq!(
            got,
            vec!["D a", "F a/x", "D a/y", "F a/y/z", "E a/y", "E a", "F a-b", "F a0",],
            "component-wise order: a/* sorts before a-b and a0, because the sort is\n\
             per-directory and NOT over `/`-joined paths"
        );
    }

    #[test]
    fn paths_are_relative_to_the_root_and_the_root_emits_nothing() {
        let got = paths(&component_order_tree());
        assert!(!got.iter().any(|p| p.contains("/r")), "paths are relative; got {got:?}");
        assert!(!got.is_empty());
    }

    #[test]
    fn every_dir_is_closed_by_a_dir_end_in_post_order() {
        let got = paths(&component_order_tree());
        let opened: Vec<&String> = got.iter().filter(|p| p.starts_with("D ")).collect();
        let closed: Vec<&String> = got.iter().filter(|p| p.starts_with("E ")).collect();
        assert_eq!(opened.len(), closed.len(), "every Dir needs a DirEnd; got {got:?}");

        // `E a` must come AFTER `F a/y/z`, or a consumer would finalize a parent's
        // metadata before writing its children -- the §30.2 violation DirEnd exists
        // to make expressible.
        let end_a = got.iter().position(|p| p == "E a").unwrap();
        let child = got.iter().position(|p| p == "F a/y/z").unwrap();
        assert!(end_a > child, "DirEnd is post-order; got {got:?}");
    }

    #[test]
    fn symlinks_are_reported_and_never_descended_into() {
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/r")).unwrap();
        fs.add_symlink("/r/link");
        // A REAL directory alongside it, with a child. Without this the test could
        // not tell "never descends into a symlink" from "never descends at all",
        // and a walk with the Dir arm deleted entirely would pass.
        fs.create_dir(Path::new("/r/real")).unwrap();
        fs.write_file("/r/real/f", b"");

        // `link` produces ONE event and it is a Symlink -- no Dir, no DirEnd --
        // while `real` produces the full Dir/child/DirEnd triple. The contrast is
        // the assertion; an exhaustive equality carries it without a second
        // redundant assert, which would be true whenever this one is.
        assert_eq!(paths(&fs), vec!["L link", "D real", "F real/f", "E real"]);
    }

    #[test]
    fn an_unreadable_directory_does_not_end_the_walk() {
        // Spec item 83: the directory's subtree is not transferred, the error is
        // reported, and the operation exits 1 -- but the WALK completes. Every
        // sibling of the failing directory must still be yielded.
        let fs = FaultFs::new();
        for d in ["/r", "/r/aaa", "/r/bbb", "/r/ccc"] {
            fs.create_dir(Path::new(d)).unwrap();
        }
        fs.write_file("/r/aaa/f", b"");
        fs.write_file("/r/bbb/f", b"");
        fs.write_file("/r/ccc/f", b"");

        // `read_dir` is called once per directory, so a one-shot fault aimed at it
        // would be eaten by the FIRST call -- the root's. `fail_nth` targets the
        // second call, which is /r/aaa. This trap made a test pass vacuously once.
        fs.fail_nth("read_dir", 2, Code::PermissionDenied, std::io::ErrorKind::PermissionDenied);

        let mut errors = Vec::new();
        let mut files = Vec::new();
        for item in walk(&fs, Path::new("/r")).unwrap() {
            match item {
                Ok(WalkEvent::File { path }) => files.push(rel(&path)),
                Err(e) => errors.push((rel(&e.path), e.cause.code)),
                Ok(_) => {}
            }
        }

        assert_eq!(errors.len(), 1, "exactly one error; got {errors:?}");
        assert_eq!(errors[0].0, "aaa");
        assert_eq!(errors[0].1, Code::PermissionDenied);
        // The siblings AFTER the failure are the whole point.
        assert_eq!(files, vec!["bbb/f".to_string(), "ccc/f".to_string()]);
    }

    #[test]
    fn the_depth_cap_skips_the_subtree_and_continues() {
        let fs = FaultFs::new();
        for d in ["/r", "/r/a", "/r/a/b", "/r/a/b/c"] {
            fs.create_dir(Path::new(d)).unwrap();
        }
        fs.write_file("/r/a/b/c/deep", b"");
        fs.write_file("/r/sibling", b"");

        // Cap of 2: `a` (1) and `a/b` (2) are entered; `a/b/c` is refused.
        let mut errors = Vec::new();
        let mut files = Vec::new();
        for item in walk_with_depth(&fs, Path::new("/r"), 2).unwrap() {
            match item {
                Ok(WalkEvent::File { path }) => files.push(rel(&path)),
                Err(e) => errors.push(rel(&e.path)),
                Ok(_) => {}
            }
        }

        assert_eq!(errors, vec!["a/b/c".to_string()]);
        assert!(!files.iter().any(|f| f.contains("deep")), "subtree skipped; got {files:?}");
        // Not an abort: the sibling after the refusal still arrives.
        assert_eq!(files, vec!["sibling".to_string()]);
    }

    #[test]
    fn the_default_cap_is_256() {
        assert_eq!(DEFAULT_MAX_DEPTH, 256);
    }

    #[test]
    fn a_directory_that_re_enters_an_ancestor_is_refused() {
        // The measured case is `mount --bind a a/b`, where `a` and `a/b` share a
        // dev+ino. `set_identity` reproduces it with no mount and no privilege.
        let fs = FaultFs::new();
        for d in ["/r", "/r/a", "/r/a/b"] {
            fs.create_dir(Path::new(d)).unwrap();
        }
        fs.write_file("/r/a/marker", b"");
        let shared = ObjectId { volume: 1, index: 9999 };
        fs.set_identity("/r/a", FileIdentity::Strong(shared));
        fs.set_identity("/r/a/b", FileIdentity::Strong(shared));

        let mut errors = Vec::new();
        let mut dirs = Vec::new();
        for item in walk(&fs, Path::new("/r")).unwrap() {
            match item {
                Ok(WalkEvent::Dir { path, .. }) => dirs.push(rel(&path)),
                Err(e) => errors.push(rel(&e.path)),
                _ => {}
            }
        }

        assert_eq!(dirs, vec!["a".to_string()], "b is never entered; got {dirs:?}");
        assert_eq!(errors, vec!["a/b".to_string()]);
    }

    #[test]
    fn the_root_is_in_the_ancestor_set_from_the_start() {
        // `mount --bind a a/b` where the re-entered directory IS the root. A walk
        // that only tracked directories it emitted events for would copy the whole
        // tree twice -- the exact defect the check exists to prevent.
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/r")).unwrap();
        fs.create_dir(Path::new("/r/loop")).unwrap();
        let shared = ObjectId { volume: 1, index: 7 };
        fs.set_identity("/r", FileIdentity::Strong(shared));
        fs.set_identity("/r/loop", FileIdentity::Strong(shared));

        let errors: Vec<String> = walk(&fs, Path::new("/r"))
            .unwrap()
            .filter_map(|i| i.err())
            .map(|e| rel(&e.path))
            .collect();
        assert_eq!(errors, vec!["loop".to_string()]);
    }

    #[test]
    fn a_weak_identity_is_not_compared_at_all() {
        // A comparison is no stronger than its weaker operand. Weak and Unavailable
        // behave identically: a value that cannot be trusted is worth no more than
        // one that is absent. PR 3 owns the warning; PR 2 yields the identity on the
        // Dir event and lets the consumer decide.
        let fs = FaultFs::new();
        for d in ["/r", "/r/a", "/r/a/b"] {
            fs.create_dir(Path::new(d)).unwrap();
        }
        let shared = ObjectId { volume: 1, index: 5 };
        fs.set_identity("/r/a", FileIdentity::Weak(shared));
        fs.set_identity("/r/a/b", FileIdentity::Weak(shared));

        let errors: Vec<String> = walk(&fs, Path::new("/r"))
            .unwrap()
            .filter_map(|i| i.err())
            .map(|e| rel(&e.path))
            .collect();
        assert!(errors.is_empty(), "weak identity is not compared; got {errors:?}");
    }

    #[test]
    fn the_dir_event_carries_the_identity_for_pr_3_to_act_on() {
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/r")).unwrap();
        fs.create_dir(Path::new("/r/w")).unwrap();
        fs.set_identity("/r/w", FileIdentity::Unavailable);

        let got: Vec<FileIdentity> = walk(&fs, Path::new("/r"))
            .unwrap()
            .filter_map(|i| i.ok())
            .filter_map(|e| match e {
                WalkEvent::Dir { identity, .. } => Some(identity),
                _ => None,
            })
            .collect();
        assert_eq!(got, vec![FileIdentity::Unavailable]);
    }

    #[test]
    fn a_read_dir_failure_after_pushing_an_ancestor_leaves_the_set_balanced() {
        // Without the unwind, `a`'s id stays in the ancestor set forever and a later
        // directory that legitimately reuses the id is refused as a FALSE cycle.
        let fs = FaultFs::new();
        for d in ["/r", "/r/a", "/r/b"] {
            fs.create_dir(Path::new(d)).unwrap();
        }
        let id = ObjectId { volume: 1, index: 42 };
        fs.set_identity("/r/a", FileIdentity::Strong(id));
        fs.set_identity("/r/b", FileIdentity::Strong(id));
        // Fail read_dir on its SECOND call, which is /r/a.
        fs.fail_nth("read_dir", 2, Code::PermissionDenied, std::io::ErrorKind::PermissionDenied);

        let errors: Vec<String> = walk(&fs, Path::new("/r"))
            .unwrap()
            .filter_map(|i| i.err())
            .map(|e| rel(&e.path))
            .collect();
        // `a` fails on read_dir. `b` must NOT then be refused as a cycle.
        assert_eq!(errors, vec!["a".to_string()], "b must not be a false cycle; got {errors:?}");
    }

    #[test]
    fn an_entry_listed_as_a_directory_that_is_no_longer_one_is_refused() {
        // §149.4: "the scanner must verify that the object being entered remains
        // consistent with the planned directory identity." read_dir typed this as a
        // Dir; metadata disagrees; therefore the object changed under the scan.
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/r")).unwrap();
        fs.create_dir(Path::new("/r/swap")).unwrap();
        // The listing still says Dir, because `directories` still holds it, while
        // `types` now reports a symlink -- exactly the split the swap produces.
        fs.set_type("/r/swap", FileType::Symlink);

        let errors: Vec<(String, Code)> = walk(&fs, Path::new("/r"))
            .unwrap()
            .filter_map(|i| i.err())
            .map(|e| (rel(&e.path), e.cause.code))
            .collect();
        assert_eq!(errors, vec![("swap".to_string(), Code::DirectoryChangedDuringScan)]);
    }

    /// Build an `OsString` from raw bytes, as far as the platform allows.
    ///
    /// The two arms are NOT equivalent, and that is stated rather than hidden. On
    /// Unix a name IS bytes, so this is exact and the test below really does cover
    /// non-UTF-8 ordering. On Windows a name is WTF-16 at the OS level and cannot
    /// hold an arbitrary byte at all, so the bytes go through a lossy conversion and
    /// the test covers NON-ASCII ordering instead. The ORDERING property is what is
    /// under test on both; the invalidity is not.
    #[cfg(unix)]
    fn os_from_bytes(b: &[u8]) -> std::ffi::OsString {
        use std::os::unix::ffi::OsStringExt;
        std::ffi::OsString::from_vec(b.to_vec())
    }

    #[cfg(windows)]
    fn os_from_bytes(b: &[u8]) -> std::ffi::OsString {
        std::ffi::OsString::from(String::from_utf8_lossy(b).into_owned())
    }

    #[test]
    fn non_utf8_names_sort_by_their_encoded_bytes() {
        // The whole reason FaultFs was rekeyed from String to PathBuf: under
        // `display()` these two names collapsed to one key.
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/r")).unwrap();

        // 0xFF is not valid UTF-8 in either direction.
        let hi = os_from_bytes(&[b'z', 0xFF]);
        let lo = os_from_bytes(&[b'a', 0xFF]);
        fs.write_file(Path::new("/r").join(&lo), b"");
        fs.write_file(Path::new("/r").join(&hi), b"");

        let got: Vec<std::ffi::OsString> = walk(&fs, Path::new("/r"))
            .unwrap()
            .filter_map(|i| i.ok())
            .filter_map(|e| match e {
                WalkEvent::File { path } => path.file_name().map(|n| n.to_os_string()),
                _ => None,
            })
            .collect();
        assert_eq!(got, vec![lo, hi], "ascending by encoded bytes");
    }

    /// A deliberately BROKEN adapter whose `read_dir` returns an absolute name,
    /// violating the contract `FileSystem::read_dir` documents. It exists so the
    /// refusal can be exercised at all: neither in-tree implementor can produce such
    /// a name, because both build it from `file_name()`.
    struct NeverHandle;

    impl std::io::Write for NeverHandle {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl flux_fs::FileHandle for NeverHandle {
        fn sync_all(&self) -> Result<()> {
            Ok(())
        }
    }

    struct EscapingFs;

    impl FileSystem for EscapingFs {
        type Reader = std::io::Empty;
        type Writer = NeverHandle;

        fn read_dir(&self, _: &Path) -> Result<Vec<DirEntry>> {
            Ok(vec![DirEntry {
                name: std::ffi::OsString::from("/etc/shadow"),
                file_type: FileType::File,
            }])
        }

        fn metadata(&self, _: &Path) -> Result<flux_fs::Metadata> {
            Ok(flux_fs::Metadata {
                len: 0,
                file_type: FileType::Dir,
                permissions: None,
                modified: None,
                identity: FileIdentity::Unavailable,
            })
        }

        fn open_read(&self, _: &Path) -> Result<Self::Reader> {
            unimplemented!("EscapingFs exists only to break read_dir")
        }
        fn create_new(&self, _: &Path) -> Result<Self::Writer> {
            unimplemented!("EscapingFs exists only to break read_dir")
        }
        fn set_times(&self, _: &Self::Writer, _: Option<std::time::SystemTime>) -> Result<()> {
            unimplemented!("EscapingFs exists only to break read_dir")
        }
        fn set_permissions(&self, _: &Self::Writer, _: Option<flux_fs::Perms>) -> Result<()> {
            unimplemented!("EscapingFs exists only to break read_dir")
        }
        fn rename_replace(&self, _: &Path, _: &Path) -> Result<()> {
            unimplemented!("EscapingFs exists only to break read_dir")
        }
        fn rename_no_replace(&self, _: &Path, _: &Path) -> Result<()> {
            unimplemented!("EscapingFs exists only to break read_dir")
        }
        fn remove_file(&self, _: &Path) -> Result<()> {
            unimplemented!("EscapingFs exists only to break read_dir")
        }
        fn create_dir(&self, _: &Path) -> Result<()> {
            unimplemented!("EscapingFs exists only to break read_dir")
        }
    }

    #[test]
    #[should_panic(expected = "not a single component")]
    fn an_adapter_returning_a_non_component_name_panics_rather_than_escaping() {
        // Without the guard this walk would yield `/etc/shadow` - `join` discards the
        // base on an absolute component, so the iterator leaves its own root. It
        // panics rather than yielding an Err item because every Err here is
        // NON-terminal, so a broken adapter would otherwise silently omit entries
        // while the suite stayed green.
        let fs = EscapingFs;
        let _: Vec<_> = walk(&fs, Path::new("/r")).unwrap().collect();
    }

    #[test]
    fn a_plain_name_is_one_component_and_the_escaping_shapes_are_not() {
        use std::ffi::OsStr;
        assert!(is_one_component(OsStr::new("ok.txt")));
        assert!(!is_one_component(OsStr::new("/etc/shadow")));
        assert!(!is_one_component(OsStr::new("a/b")));
        assert!(!is_one_component(OsStr::new("..")));
        assert!(!is_one_component(OsStr::new(".")));
        assert!(!is_one_component(OsStr::new("")));
    }
}
