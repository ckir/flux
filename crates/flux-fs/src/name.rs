//! One-component name validation, shared by every `DirHandle` implementation.

use crate::{Code, FsError, Result};
use std::ffi::OsStr;

/// Refuse anything that is not exactly one path component, BEFORE any syscall.
///
/// A handle-relative call takes one component by construction. `OsStr` cannot
/// enforce that, so this does: a caller that passes `"a/b"` would otherwise hand
/// the middle component back to kernel path resolution, which is precisely the
/// resolution §149.7 exists to remove.
///
/// Both separators are rejected on every platform, not just the native one. A
/// name carrying a backslash is not a valid single component on Windows, and on
/// POSIX it is a legal filename that this project has no reason to create and
/// every reason to refuse from a caller that thought it was a separator.
pub fn check_component(name: &OsStr) -> Result<()> {
    let refuse = |why: &str| {
        Err(FsError::new(
            Code::SafetyRejected,
            std::io::Error::new(std::io::ErrorKind::InvalidInput, why.to_string()),
        ))
    };

    // A LENGTH BOUND, and it is a correctness fix rather than hygiene.
    //
    // Windows addresses a handle-relative name through a `UNICODE_STRING`, whose
    // `Length` is a **u16 counting BYTES**. The arms built it as
    // `(wide.len() * 2) as u16`, which WRAPS. MEASURED, on NTFS, before this check
    // existed: `create_new` asked for a 32868-character name -- 65736 bytes, which
    // wraps to 200 -- and the kernel was handed the first 100 characters. It created
    // that 100-character file and returned Ok. A silent write to a name the caller
    // never asked for, reported as success.
    //
    // That is the same defect class as the interior-NUL truncation already fixed in
    // the path-based arm, and it is worse here: this cut exists so a destination
    // write lands exactly where the caller said, and a wrapped length is a way for it
    // not to, with no error anywhere.
    //
    // The bound is checked BEFORE the `to_str` branch on purpose. The non-UTF-8 path
    // does nothing at all on Windows -- an unpaired surrogate is its only reachable
    // input -- so a check placed inside that branch would leave exactly the names
    // that skip every other rule unbounded.
    //
    // 32767 UTF-16 units is far above any real filesystem's component limit (255 is
    // the usual cap), so nothing legitimate is refused. POSIX has no `UNICODE_STRING`
    // and does not need the bound; it applies it anyway, because two arms that refuse
    // the same names are the point of this cut.
    const MAX_COMPONENT_UTF16_UNITS: usize = (u16::MAX as usize) / 2;
    #[cfg(windows)]
    let units = {
        use std::os::windows::ffi::OsStrExt;
        name.encode_wide().count()
    };
    #[cfg(not(windows))]
    let units = name.len();
    if units > MAX_COMPONENT_UTF16_UNITS {
        return refuse("a path component is too long to address safely");
    }

    let Some(s) = name.to_str() else {
        // A non-UTF-8 name is fine as a NAME; it just cannot be scanned as `str`.
        // Fall back to the byte view, which is enough to find separators.
        return check_component_bytes(name);
    };

    if s.is_empty() {
        return refuse("an empty name is not a path component");
    }
    if s == "." || s == ".." {
        return refuse("`.` and `..` are not publishable components");
    }
    if s.contains('/') || s.contains('\\') {
        return refuse("a path component may not contain a separator");
    }
    Ok(())
}

#[cfg(unix)]
fn check_component_bytes(name: &OsStr) -> Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let b = name.as_bytes();
    if b.is_empty() || b == b"." || b == b".." || b.contains(&b'/') || b.contains(&b'\\') {
        return Err(FsError::new(
            Code::SafetyRejected,
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "not a single path component".to_string(),
            ),
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
fn check_component_bytes(name: &OsStr) -> Result<()> {
    // Windows `OsStr` is WTF-16; an unpaired surrogate is the only way to reach
    // here, and such a name carries no separator by construction.
    let _ = name;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    #[test]
    fn a_component_too_long_for_a_unicode_string_is_refused() {
        // MEASURED before this bound existed: a 32868-character name has a UTF-16
        // byte length of 65736, which wraps a u16 to 200 -- so NtCreateFile was
        // handed the first 100 characters, CREATED that file, and returned Ok. The
        // caller asked for one name and a different one appeared, reported as
        // success. 32868 is the exact value that reproduced it.
        let long: String = "b".repeat(32868);
        let err = check_component(OsStr::new(&long)).unwrap_err();
        assert_eq!(err.code, Code::SafetyRejected);

        // The bound is far above any real component limit, so ordinary names pass.
        assert!(check_component(OsStr::new(&"b".repeat(255))).is_ok());
    }

    #[test]
    fn a_plain_component_is_accepted() {
        assert!(check_component(OsStr::new("payload.txt")).is_ok());
        assert!(check_component(OsStr::new("a file with spaces")).is_ok());
    }

    #[test]
    fn a_separator_is_refused_before_any_syscall() {
        // THE reason this helper exists. A handle-relative call takes ONE component;
        // a caller that slips "a/b" through would hand the middle component back to
        // kernel path resolution, which is the hole §149.7 closes.
        for bad in ["a/b", "a\\b", "/abs", "nested/deep/path"] {
            let err = check_component(OsStr::new(bad)).unwrap_err();
            assert_eq!(err.code, crate::Code::SafetyRejected, "{bad} must be refused");
        }
    }

    #[test]
    fn dot_and_dotdot_are_refused() {
        // `..` escapes the parent, which is the whole point of holding a handle to it.
        for bad in [".", ".."] {
            let err = check_component(OsStr::new(bad)).unwrap_err();
            assert_eq!(err.code, crate::Code::SafetyRejected, "{bad} must be refused");
        }
    }

    #[test]
    fn an_empty_name_is_refused() {
        let err = check_component(OsStr::new("")).unwrap_err();
        assert_eq!(err.code, crate::Code::SafetyRejected);
    }
}
