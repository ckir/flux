// Copyright (c) 2026 Costas Kirgoussios
//
// This software is licensed under the PolyForm Noncommercial License 1.0.0.
// You may obtain a copy of the License at:
// https://polyformproject.org/licenses/noncommercial/1.0.0/

//! Planning the roots of a several-source run (spec 3.2, 3.4, R6, L5): which names the sources take under the
//! destination, in which order, and how many a run may carry.

use super::RunError;
use crate::lock::{LockCode, Refusal, prefix_lock_fits};
use crate::tree::open_writers_cap;
use flux_fs::{FluxPathKey, check_component};
use std::ffi::{OsStr, OsString};
use std::path::PathBuf;

/// What a source is: a directory tree or a single file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    Folder,
    File,
}

/// One source of a several-source run and the name it takes under the destination.
#[derive(Debug, Clone)]
pub struct SourceSpec {
    pub src: PathBuf,
    pub name: OsString,
    pub kind: SourceKind,
}

/// The most sources one run carries: `open_writers_cap`'s shape, applied to the soft descriptor limit.
pub fn source_cap(descriptor_limit: Option<u64>) -> usize {
    open_writers_cap(descriptor_limit)
}

fn refuse(code: LockCode, detail: String) -> RunError {
    RunError::Refused {
        refusal: Box::new(Refusal { code, holder: None, detail }),
        changed: false,
        not_removed: None,
    }
}

/// The name's units (bytes on Unix, UTF-16 units on Windows), ASCII-lowercased, so the control-plane rules also
/// bind a name that is not valid Unicode.
#[cfg(unix)]
fn folded_units(name: &OsStr) -> Vec<u16> {
    use std::os::unix::ffi::OsStrExt;
    name.as_bytes().iter().map(|b| u16::from(b.to_ascii_lowercase())).collect()
}

#[cfg(windows)]
fn folded_units(name: &OsStr) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    name.encode_wide()
        .map(|u| match u8::try_from(u) {
            Ok(b) if b.is_ascii() => u16::from(b.to_ascii_lowercase()),
            _ => u,
        })
        .collect()
}

fn is_control_plane(name: &OsStr) -> bool {
    let units = folded_units(name);
    let ascii = |s: &str| s.bytes().map(u16::from).collect::<Vec<u16>>();
    [".flux", ".flux-dir.lock", ".flux-root.lock"].iter().any(|n| units == ascii(n))
        || units.ends_with(&ascii(".flux-lock"))
}

#[cfg(windows)]
fn windows_reserved(name: &OsStr) -> bool {
    use std::os::windows::ffi::OsStrExt;
    // A trailing dot or space can follow a lone surrogate; a device name is ASCII, so it cannot.
    if matches!(name.encode_wide().last(), Some(0x2e | 0x20)) {
        return true;
    }
    let Some(name) = name.to_str() else { return false };
    let stem = name.split('.').next().unwrap_or("").trim_end_matches(' ').to_ascii_uppercase();
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && matches!(stem.as_bytes()[3], b'1'..=b'9'))
}

/// Check every name, refuse two sources that take the same name, and return the specs in prefix-key order.
pub(crate) fn plan_roots(specs: Vec<SourceSpec>) -> Result<Vec<SourceSpec>, RunError> {
    let mut keyed: Vec<(Vec<u8>, SourceSpec)> = Vec::with_capacity(specs.len());
    for spec in specs {
        let shown = spec.src.display();
        let invalid = |why: &str| {
            refuse(
                LockCode::PathComponentInvalid,
                format!("the name {:?} for the source {shown} is not usable: {why}", spec.name),
            )
        };
        if let Err(e) = check_component(&spec.name) {
            return Err(invalid(&e.to_string()));
        }
        #[cfg(windows)]
        if windows_reserved(&spec.name) {
            return Err(invalid("a reserved or trailing-dot/space Windows name"));
        }
        if is_control_plane(&spec.name) {
            return Err(refuse(
                LockCode::ControlPlaneNamespaceConflict,
                format!(
                    "the name {:?} for the source {shown} is in the namespace Flux keeps for itself",
                    spec.name
                ),
            ));
        }
        if !prefix_lock_fits(&spec.name) {
            return Err(invalid("no room beside it for the names Flux keeps for its lock"));
        }
        let key = FluxPathKey::from_relative(std::path::Path::new(&spec.name))
            .map_err(|e| invalid(&e.to_string()))?
            .0;
        if let Some((_, first)) = keyed.iter().find(|(k, _)| *k == key) {
            return Err(refuse(
                LockCode::DestinationNamespaceCollision,
                format!(
                    "the sources {} and {} both take the name {:?}",
                    first.src.display(),
                    shown,
                    spec.name
                ),
            ));
        }
        keyed.push((key, spec));
    }
    keyed.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(keyed.into_iter().map(|(_, s)| s).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    fn spec(src: &str, name: &str) -> SourceSpec {
        SourceSpec { src: PathBuf::from(src), name: OsString::from(name), kind: SourceKind::Folder }
    }

    fn code_of(r: Result<Vec<SourceSpec>, RunError>) -> (LockCode, String) {
        match r {
            Err(RunError::Refused { refusal, changed, not_removed }) => {
                assert!(!changed);
                assert!(not_removed.is_none());
                assert!(refusal.holder.is_none());
                (refusal.code, refusal.detail)
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn sorts_by_prefix_key() {
        let out =
            plan_roots(vec![spec("/s/c", "c"), spec("/s/a", "a"), spec("/s/b", "b")]).unwrap();
        let names: Vec<&OsStr> = out.iter().map(|s| s.name.as_os_str()).collect();
        assert_eq!(names, ["a", "b", "c"]);
    }

    #[test]
    fn byte_equal_names_collide_and_name_both_sources() {
        let (code, detail) = code_of(plan_roots(vec![spec("/one/x", "x"), spec("/two/x", "x")]));
        assert_eq!(code, LockCode::DestinationNamespaceCollision);
        assert!(detail.contains("/one/x") && detail.contains("/two/x"), "{detail}");
    }

    #[test]
    fn control_plane_names_conflict() {
        for name in
            [".flux", ".FLUX", ".flux-dir.lock", ".flux-root.lock", "x.flux-lock", "X.FLUX-LOCK"]
        {
            let (code, detail) = code_of(plan_roots(vec![spec("/s/src", name)]));
            assert_eq!(code, LockCode::ControlPlaneNamespaceConflict, "{name}");
            assert!(detail.contains("/s/src"), "{detail}");
        }
    }

    #[test]
    fn recovery_name_length_boundary() {
        let fits = "a".repeat(205);
        let over = "a".repeat(206);
        assert!(plan_roots(vec![spec("/s/f", &fits)]).is_ok());
        let (code, detail) = code_of(plan_roots(vec![spec("/s/o", &over)]));
        assert_eq!(code, LockCode::PathComponentInvalid);
        assert!(detail.contains("/s/o"), "{detail}");
    }

    #[test]
    fn invalid_components_are_refused() {
        for name in ["a/b", ""] {
            let (code, _) = code_of(plan_roots(vec![spec("/s/src", name)]));
            assert_eq!(code, LockCode::PathComponentInvalid, "{name:?}");
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_device_and_trailing_names_are_refused() {
        for name in ["CON", "nul", "a.", "a "] {
            let (code, _) = code_of(plan_roots(vec![spec("/s/src", name)]));
            assert_eq!(code, LockCode::PathComponentInvalid, "{name:?}");
        }
    }

    #[test]
    fn source_cap_table() {
        for (limit, want) in
            [(None, 256), (Some(1024), 256), (Some(256), 64), (Some(3), 1), (Some(0), 1)]
        {
            assert_eq!(source_cap(limit), want, "{limit:?}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_non_utf8_name_ending_in_flux_lock_is_a_control_plane_conflict() {
        use std::os::unix::ffi::OsStringExt;
        for raw in [&b"\xff.flux-lock"[..], &b"\xff.FLUX-LOCK"[..]] {
            let mut sp = spec("/s/src", "x");
            sp.name = OsString::from_vec(raw.to_vec());
            match plan_roots(vec![sp]) {
                Err(RunError::Refused { refusal, changed, .. }) => {
                    assert_eq!(refusal.code, LockCode::ControlPlaneNamespaceConflict, "{raw:?}");
                    assert!(!changed);
                }
                other => panic!("expected a refusal for {raw:?}, got {other:?}"),
            }
        }
    }
}
