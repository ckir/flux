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
                frame.entries.next().map(|e| (frame.rel.join(&e.name), e.file_type))
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

        let entries = match self.fs.read_dir(&abs) {
            Ok(v) => sorted(v),
            Err(cause) => return Err(WalkError { path: rel, cause }),
        };

        self.stack.push(Frame {
            rel: rel.clone(),
            entries: entries.into_iter(),
            pushed_ancestor: false,
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
}
