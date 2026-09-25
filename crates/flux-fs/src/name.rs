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
