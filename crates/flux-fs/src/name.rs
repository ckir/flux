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
        //
        // THE OTHER SIDE OF THIS FORK MUST ENFORCE THE SAME RULES, and for a while it
        // enforced none: it returned Ok unconditionally under a comment claiming a
        // name reaching it could not carry a separator. It could, and a lone
        // surrogate was enough to skip every check below. This comment used to say the
        // fallback was "enough to find separators", which understated what it owes --
        // the sort of understatement that let that hole survive review. It owes
        // everything below except `.` and `..`, which are valid UTF-8 and so can only
        // arrive on the other branch.
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
    // An interior NUL is not part of a name on either platform, and refusing it here
    // is defence in depth rather than a fix for an observed hole. MEASURED: both arms
    // already refuse it -- POSIX with InvalidInput from rustix, Windows with a kernel
    // error -- because a UNICODE_STRING is length-counted and so does not truncate at
    // a NUL the way the NUL-TERMINATED path API does. The path-based arm was not so
    // lucky: a `to` of "pub\0lish" was measured to publish at "pub" and return Ok.
    //
    // Two kernels happening to agree is a weaker guarantee than this function
    // stating the rule, and it left the two arms answering different ErrorKinds for
    // the same rejected name. Refusing at the choke point makes the answer one
    // answer, and makes it before any syscall.
    if s.contains('\0') {
        return refuse("a path component may not contain an interior NUL");
    }
    check_no_stream_separator(s)
}

/// A colon does not belong in a Windows component, and this check IS platform-specific
/// where the length bound above deliberately is not.
///
/// The length bound refuses nothing any filesystem accepts, so both arms apply it and
/// agree. A colon is the opposite: it is an ordinary, legal character in a POSIX
/// filename, and refusing it there would reject names people really have. On Windows
/// it separates a file from one of its alternate data STREAMS, so a name carrying one
/// is not one component -- it addresses something inside another object.
///
/// MEASURED: `create_new("host:stream")` returned Ok and wrote a stream inside the
/// existing file `host`, leaving the directory with one entry and no `host:stream` in
/// it. The caller asked for one name and a different object was written, reported as
/// success -- the same shape as the u16 length wrap, and the same reason to refuse it
/// at the choke point rather than deeper down.
#[cfg(windows)]
fn check_no_stream_separator(s: &str) -> Result<()> {
    if s.contains(':') {
        return Err(FsError::new(
            Code::SafetyRejected,
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "a path component may not contain a stream separator".to_string(),
            ),
        ));
    }
    Ok(())
}

#[cfg(not(windows))]
fn check_no_stream_separator(s: &str) -> Result<()> {
    // A colon is a legal character in a POSIX filename; see the note above.
    let _ = s;
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
    // THIS USED TO RETURN Ok UNCONDITIONALLY, under a comment asserting that an
    // unpaired surrogate is the only way to reach here and that "such a name carries
    // no separator by construction". The first half is true and the second does not
    // follow: an unpaired surrogate can sit in a name beside any other character.
    //
    // MEASURED: a name of "host", one lone high surrogate, and ":stream" has
    // `to_str() == None`, so it never enters the UTF-8 path above where the separator
    // and stream-separator checks live -- and `create_new` ACCEPTED it and wrote an
    // object the caller had not named. Every rule this function exists to enforce was
    // one unpaired surrogate away from being skipped.
    //
    // The same rules are therefore applied to the code units directly. Scanning UTF-16
    // units rather than `char`s is exactly right here: every character being rejected
    // is ASCII, so it is its own single unit, and no surrogate can be mistaken for one.
    use std::os::windows::ffi::OsStrExt;

    let refuse = |why: &str| {
        Err(FsError::new(
            Code::SafetyRejected,
            std::io::Error::new(std::io::ErrorKind::InvalidInput, why.to_string()),
        ))
    };

    let mut empty = true;
    for unit in name.encode_wide() {
        empty = false;
        match unit {
            // '/' and '\\'
            0x2F | 0x5C => return refuse("a path component may not contain a separator"),
            // ':' -- see `check_no_stream_separator` for why this is Windows-only.
            0x3A => return refuse("a path component may not contain a stream separator"),
            // NUL -- see the UTF-8 path for why this is stated rather than left to
            // the kernel.
            0x00 => return refuse("a path component may not contain an interior NUL"),
            _ => {}
        }
    }
    if empty {
        return refuse("an empty name is not a path component");
    }
    // `.` and `..` need no check here: both are valid UTF-8, so a name equal to
    // either one took the branch above rather than this one.
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    #[test]
    fn an_interior_nul_is_refused_on_every_platform() {
        // Defence in depth, stated rather than delegated. MEASURED before this: both
        // arms already refused such a name, POSIX with InvalidInput from rustix and
        // Windows with a kernel error, because a UNICODE_STRING is length-counted and
        // does not truncate at a NUL the way the NUL-terminated path API does -- the
        // path-based arm WAS truncated by one, publishing "pub\0lish" at "pub" and
        // returning Ok. Two kernels agreeing is weaker than this function saying so,
        // and it left the arms answering different kinds for the same rejected name.
        let err = check_component(OsStr::new("pub\0lish")).unwrap_err();
        assert_eq!(err.code, Code::SafetyRejected);
        assert!(check_component(OsStr::new("publish")).is_ok());
    }

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
