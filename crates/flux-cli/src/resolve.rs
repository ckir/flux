//! From the two command-line paths to what the engine is asked to do (cut 5): §4.1's
//! one-source mapping, K7 (a symlink SOURCE is never followed) and K6
//! (canonicalization for a folder copy).

use flux_core::run::{SourceKind, SourceSpec};
use flux_fs::{Code, FsError};
use std::ffi::{OsStr, OsString};
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};

/// What the engine is asked to do.
#[derive(Debug, PartialEq, Eq)]
pub enum Job {
    /// A folder's CONTENTS onto `dst` (§4.1). Both paths canonical (K6), the absent
    /// remainder of `dst` appended.
    Tree { src: PathBuf, dst: PathBuf },
    /// One file to `dst`, as given (after §4.1's `DEST/<name>`). `target_existed` feeds
    /// only the JSON `files_overwritten`.
    File { src: PathBuf, dst: PathBuf, target_existed: bool },
}

/// Why nothing was handed to the engine.
#[derive(Debug)]
pub enum Stop {
    /// Exit 2 with this message: a folder onto an existing file (§4.1).
    Usage(String),
    /// K7: SOURCE is a symlink, which is never followed.
    SymlinkSource,
    /// Exit 1 with this line: a missing SOURCE, or a resolution failure.
    Failed(String),
}

/// §4.1 for one source.
pub fn job(source: &Path, dest: &Path) -> Result<Job, Stop> {
    let meta = std::fs::symlink_metadata(source).map_err(|e| failed(source, &e))?;
    if meta.file_type().is_symlink() {
        return Err(Stop::SymlinkSource);
    }
    if meta.is_dir() {
        // DEST is classified FOLLOWING links, because the engine follows a user-named
        // destination (§149.7). Absent or dangling is "anything else".
        if matches!(std::fs::metadata(dest), Ok(m) if !m.is_dir()) {
            return Err(Stop::Usage(format!(
                "a folder cannot be copied onto the existing file {}",
                dest.display()
            )));
        }
        let src = std::fs::canonicalize(source).map_err(|e| failed(source, &e))?;
        let dst = canonical_with_remainder(dest).map_err(|e| failed(dest, &e))?;
        return Ok(Job::Tree { src, dst });
    }
    let into_folder =
        ends_with_separator(dest.as_os_str()) || std::fs::metadata(dest).is_ok_and(|m| m.is_dir());
    let dst = if into_folder {
        let Some(name) = source.file_name() else {
            return Err(no_file_name(source));
        };
        dest.join(name)
    } else {
        dest.to_path_buf()
    };
    let target_existed = std::fs::symlink_metadata(&dst).is_ok();
    Ok(Job::File { src: source.to_path_buf(), dst, target_existed })
}

/// §4.1's last row for two or more sources (spec 3.2): each is resolved as `job` resolves one, in this order:
/// `symlink_metadata` on the path as given, then its kind, then canonicalisation for the name (the canonical path's
/// final component). DEST is classified as `job` does, and returned canonical with its absent remainder appended.
/// `cap` is `source_cap`'s figure.
pub fn several(
    sources: &[PathBuf],
    dest: &Path,
    cap: usize,
) -> Result<(Vec<SourceSpec>, PathBuf), Stop> {
    if sources.len() < 2 {
        return Err(Stop::Usage("several sources need at least two sources".to_string()));
    }
    if sources.len() > cap {
        return Err(Stop::Usage(format!(
            "at most {cap} sources are accepted in one run; {} were given",
            sources.len()
        )));
    }
    let mut specs = Vec::with_capacity(sources.len());
    for source in sources {
        let meta = std::fs::symlink_metadata(source).map_err(|e| failed(source, &e))?;
        if meta.file_type().is_symlink() {
            return Err(Stop::SymlinkSource);
        }
        let kind = if meta.is_dir() {
            SourceKind::Folder
        } else if meta.is_file() {
            SourceKind::File
        } else {
            return Err(Stop::Failed(format!(
                "{}: {}: not a regular file or a folder",
                Code::SpecialFileUnsupported.as_str(),
                source.display()
            )));
        };
        let src = std::fs::canonicalize(source).map_err(|e| failed(source, &e))?;
        let Some(name) = src.file_name().map(OsStr::to_os_string) else {
            return Err(no_file_name(source));
        };
        specs.push(SourceSpec { src, name, kind });
    }
    if matches!(std::fs::metadata(dest), Ok(m) if !m.is_dir()) {
        return Err(Stop::Usage(format!(
            "several sources cannot be copied onto the existing file {}",
            dest.display()
        )));
    }
    let dst = canonical_with_remainder(dest).map_err(|e| failed(dest, &e))?;
    Ok((specs, dst))
}

/// The first of `sources`, in argument order, that is a symlink: the one `several` named `Stop::SymlinkSource` for
/// (it stops at the first failure, so every earlier source passed).
pub fn first_symlink(sources: &[PathBuf]) -> Option<&Path> {
    sources
        .iter()
        .find(|s| std::fs::symlink_metadata(s).is_ok_and(|m| m.file_type().is_symlink()))
        .map(PathBuf::as_path)
}

fn no_file_name(source: &Path) -> Stop {
    Stop::Failed(format!(
        "{}: {}: the source has no file name",
        Code::IoError.as_str(),
        source.display()
    ))
}

fn failed(path: &Path, e: &io::Error) -> Stop {
    Stop::Failed(format!("{}: {}: {e}", FsError::classify(e).as_str(), path.display()))
}

/// Tested on the RAW argument: `Path` drops a trailing separator.
pub fn ends_with_separator(p: &OsStr) -> bool {
    let last = p.as_encoded_bytes().last();
    last == Some(&b'/') || (cfg!(windows) && last == Some(&b'\\'))
}

/// K6: canonicalize the nearest existing ancestor and append the absent remainder
/// lexically (components that do not exist cannot be links). Climbs ONLY on
/// `NotFound` (absent, or a dangling link); any other error is returned. A relative
/// path is joined to the current directory first, so the climb ends at a root.
pub fn canonical_with_remainder(p: &Path) -> io::Result<PathBuf> {
    let abs = if p.is_absolute() { p.to_path_buf() } else { std::env::current_dir()?.join(p) };
    let mut rest: Vec<OsString> = Vec::new();
    let mut cur: &Path = &abs;
    loop {
        match std::fs::canonicalize(cur) {
            Ok(mut c) => {
                c.extend(rest.iter().rev());
                return Ok(c);
            }
            Err(e) if e.kind() == ErrorKind::NotFound => {
                let (Some(name), Some(parent)) = (cur.file_name(), cur.parent()) else {
                    return Err(e);
                };
                rest.push(name.to_owned());
                cur = parent;
            }
            Err(e) => return Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn canonical_tmp() -> (TempDir, PathBuf) {
        let d = TempDir::new().unwrap();
        let c = std::fs::canonicalize(d.path()).unwrap();
        (d, c)
    }

    #[test]
    fn a_trailing_separator_is_detected_on_the_raw_argument() {
        assert!(ends_with_separator(OsStr::new("out/")));
        assert!(!ends_with_separator(OsStr::new("out")));
        assert_eq!(ends_with_separator(OsStr::new("out\\")), cfg!(windows));
    }

    #[test]
    fn the_absent_remainder_is_appended_to_the_nearest_existing_ancestor() {
        let (_d, tmp) = canonical_tmp();
        let p = tmp.join("x").join("y");
        assert_eq!(canonical_with_remainder(&p).unwrap(), p);
        assert_eq!(canonical_with_remainder(&tmp).unwrap(), tmp);
    }

    #[test]
    fn a_folder_onto_an_existing_file_is_a_usage_error() {
        let (_d, tmp) = canonical_tmp();
        std::fs::create_dir(tmp.join("src")).unwrap();
        std::fs::write(tmp.join("f"), b"x").unwrap();
        assert!(matches!(job(&tmp.join("src"), &tmp.join("f")), Err(Stop::Usage(_))));
    }

    #[test]
    fn a_folder_becomes_a_tree_job_on_canonical_paths() {
        let (_d, tmp) = canonical_tmp();
        std::fs::create_dir(tmp.join("src")).unwrap();
        let got = job(&tmp.join("src"), &tmp.join("new").join("dst")).unwrap();
        assert_eq!(got, Job::Tree { src: tmp.join("src"), dst: tmp.join("new").join("dst") });
    }

    #[test]
    fn a_file_into_an_existing_folder_or_a_separator_path_takes_its_own_name() {
        let (_d, tmp) = canonical_tmp();
        std::fs::write(tmp.join("a"), b"x").unwrap();
        std::fs::create_dir(tmp.join("out")).unwrap();

        let into = job(&tmp.join("a"), &tmp.join("out")).unwrap();
        assert_eq!(
            into,
            Job::File { src: tmp.join("a"), dst: tmp.join("out").join("a"), target_existed: false }
        );

        let mut raw = tmp.join("missing").into_os_string();
        raw.push("/");
        let sep = job(&tmp.join("a"), Path::new(&raw)).unwrap();
        assert!(
            matches!(&sep, Job::File { dst, .. } if dst.file_name() == Some(OsStr::new("a"))),
            "{sep:?}"
        );
    }

    #[test]
    fn a_file_onto_an_existing_file_records_that_it_existed() {
        let (_d, tmp) = canonical_tmp();
        std::fs::write(tmp.join("a"), b"x").unwrap();
        std::fs::write(tmp.join("b"), b"y").unwrap();
        assert_eq!(
            job(&tmp.join("a"), &tmp.join("b")).unwrap(),
            Job::File { src: tmp.join("a"), dst: tmp.join("b"), target_existed: true }
        );
    }

    #[test]
    fn a_missing_source_is_a_failure_line_naming_it() {
        let (_d, tmp) = canonical_tmp();
        match job(&tmp.join("nope"), &tmp.join("dst")) {
            Err(Stop::Failed(line)) => {
                assert!(line.starts_with("IO_ERROR: "), "{line}");
                assert!(line.contains("nope"), "{line}");
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_source_is_never_followed() {
        let (_d, tmp) = canonical_tmp();
        std::fs::create_dir(tmp.join("dir")).unwrap();
        std::os::unix::fs::symlink(tmp.join("dir"), tmp.join("to-dir")).unwrap();
        std::os::unix::fs::symlink(tmp.join("nowhere"), tmp.join("dangling")).unwrap();
        for link in ["to-dir", "dangling"] {
            assert!(
                matches!(job(&tmp.join(link), &tmp.join("dst")), Err(Stop::SymlinkSource)),
                "{link}"
            );
        }
    }

    /// Unix only: there, a path through a FILE fails with ENOTDIR, which the climb does
    /// not treat as absent. (Windows reports it as not-found, and the climb continues.)
    #[cfg(unix)]
    #[test]
    fn a_destination_under_a_file_fails_naming_the_destination() {
        let (_d, tmp) = canonical_tmp();
        std::fs::create_dir(tmp.join("src")).unwrap();
        std::fs::write(tmp.join("f"), b"x").unwrap();
        let dest = tmp.join("f").join("x");

        match job(&tmp.join("src"), &dest) {
            Err(Stop::Failed(line)) => {
                assert!(line.contains(&dest.display().to_string()), "{line}");
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    fn paths(base: &Path, names: &[&str]) -> Vec<PathBuf> {
        names.iter().map(|n| base.join(n)).collect()
    }

    #[test]
    fn several_names_come_from_the_canonical_source() {
        let (_d, tmp) = canonical_tmp();
        std::fs::create_dir_all(tmp.join("dir").join("inner")).unwrap();
        std::fs::create_dir(tmp.join("x")).unwrap();
        std::fs::write(tmp.join("f"), b"x").unwrap();
        let mut slash = tmp.join("dir").into_os_string();
        slash.push("/");
        let sources = vec![
            tmp.join("dir").join("inner").join("."),
            tmp.join("dir").join("..").join("x"),
            PathBuf::from(slash),
            tmp.join("dir").join("..").join("f"),
        ];

        let (specs, dst) = several(&sources, &tmp.join("out"), 10).unwrap();

        let got: Vec<_> = specs.iter().map(|s| (s.name.clone(), s.kind, s.src.clone())).collect();
        assert_eq!(
            got,
            vec![
                (OsString::from("inner"), SourceKind::Folder, tmp.join("dir").join("inner")),
                (OsString::from("x"), SourceKind::Folder, tmp.join("x")),
                (OsString::from("dir"), SourceKind::Folder, tmp.join("dir")),
                (OsString::from("f"), SourceKind::File, tmp.join("f")),
            ]
        );
        assert_eq!(dst, tmp.join("out"));
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_source_is_refused_before_the_name_is_taken() {
        let (_d, tmp) = canonical_tmp();
        std::fs::create_dir(tmp.join("dir")).unwrap();
        std::fs::create_dir(tmp.join("ok")).unwrap();
        std::os::unix::fs::symlink(tmp.join("dir"), tmp.join("to-dir")).unwrap();
        std::os::unix::fs::symlink(tmp.join("nowhere"), tmp.join("dangling")).unwrap();
        for link in ["to-dir", "dangling"] {
            let sources = paths(&tmp, &["ok", link]);
            assert!(
                matches!(several(&sources, &tmp.join("out"), 10), Err(Stop::SymlinkSource)),
                "{link}"
            );
            assert_eq!(first_symlink(&sources), Some(tmp.join(link).as_path()), "{link}");
        }
        assert_eq!(first_symlink(&paths(&tmp, &["ok", "dir"])), None);
    }

    #[test]
    fn a_missing_source_is_failed_naming_it() {
        let (_d, tmp) = canonical_tmp();
        std::fs::create_dir(tmp.join("ok")).unwrap();
        match several(&paths(&tmp, &["ok", "nope"]), &tmp.join("out"), 10) {
            Err(Stop::Failed(line)) => {
                assert!(line.starts_with("IO_ERROR: "), "{line}");
                assert!(line.contains("nope"), "{line}");
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    fn dest_that_is_a_file_is_a_usage_error() {
        let (_d, tmp) = canonical_tmp();
        std::fs::create_dir(tmp.join("a")).unwrap();
        std::fs::create_dir(tmp.join("b")).unwrap();
        std::fs::write(tmp.join("f"), b"x").unwrap();
        match several(&paths(&tmp, &["a", "b"]), &tmp.join("f"), 10) {
            Err(Stop::Usage(msg)) => assert!(msg.contains("existing file"), "{msg}"),
            other => panic!("expected Usage, got {other:?}"),
        }
    }

    #[test]
    fn over_the_cap_is_a_usage_error_and_at_the_cap_is_accepted() {
        let (_d, tmp) = canonical_tmp();
        for n in ["a", "b", "c", "d"] {
            std::fs::create_dir(tmp.join(n)).unwrap();
        }
        let out = tmp.join("out");
        assert_eq!(several(&paths(&tmp, &["a", "b", "c"]), &out, 3).unwrap().0.len(), 3);
        match several(&paths(&tmp, &["a", "b", "c", "d"]), &out, 3) {
            Err(Stop::Usage(msg)) => assert!(msg.contains('3') && msg.contains('4'), "{msg}"),
            other => panic!("expected Usage, got {other:?}"),
        }
    }

    #[test]
    fn fewer_than_two_sources_is_a_usage_error() {
        let (_d, tmp) = canonical_tmp();
        std::fs::create_dir(tmp.join("a")).unwrap();
        assert!(matches!(several(&paths(&tmp, &["a"]), &tmp.join("o"), 10), Err(Stop::Usage(_))));
    }

    #[cfg(unix)]
    #[test]
    fn a_source_without_a_final_component_fails() {
        let (_d, tmp) = canonical_tmp();
        std::fs::create_dir(tmp.join("ok")).unwrap();
        let sources = vec![tmp.join("ok"), PathBuf::from("/")];
        match several(&sources, &tmp.join("out"), 10) {
            Err(Stop::Failed(line)) => {
                assert!(line.contains("no file name") && line.contains('/'), "{line}");
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }
}
