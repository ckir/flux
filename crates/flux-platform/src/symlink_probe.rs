// Copyright (c) 2026 Costas Kirgoussios
//
// This software is licensed under the PolyForm Noncommercial License 1.0.0.
// You may obtain a copy of the License at:
// https://polyformproject.org/licenses/noncommercial/1.0.0/

//! Owner-run measurement probes for cut 10a (symlink copying). Every test is `#[ignore]`, PRINTS labelled
//! results and asserts nothing except that it ran. They create nothing outside a `tempfile::TempDir`
//! (or the directory named by `FLUX_PROBE_DIR` for the Linux one) and clean up after themselves.
//!
//! Run: `cargo test -p flux-platform --lib -- --ignored symlink_probe --nocapture`

#[cfg(windows)]
mod win {
    use std::ffi::{OsStr, OsString};
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use std::path::{Path, PathBuf};
    use windows_sys::Wdk::Foundation::OBJECT_ATTRIBUTES;
    use windows_sys::Wdk::Storage::FileSystem::{
        FILE_CREATE, FILE_DIRECTORY_FILE, FILE_NON_DIRECTORY_FILE, FILE_OPEN,
        FILE_OPEN_REPARSE_POINT, FILE_RENAME_INFORMATION, FILE_RENAME_POSIX_SEMANTICS,
        FILE_RENAME_REPLACE_IF_EXISTS, FileAttributeTagInformation, FileRenameInformation,
        FileRenameInformationEx, NtCreateFile, NtQueryInformationFile, NtSetInformationFile,
    };
    use windows_sys::Win32::Foundation::{GetLastError, HANDLE, UNICODE_STRING};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateSymbolicLinkW, DELETE, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        FILE_GENERIC_WRITE, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
        SYMBOLIC_LINK_FLAG_ALLOW_UNPRIVILEGED_CREATE, SYMBOLIC_LINK_FLAG_DIRECTORY, SYNCHRONIZE,
    };
    use windows_sys::Win32::System::IO::{DeviceIoControl, IO_STATUS_BLOCK};

    /// `FSCTL_SET_REPARSE_POINT`, declared locally so the probe needs no extra windows-sys feature.
    const FSCTL_SET_REPARSE_POINT: u32 = 0x0009_00A4;
    const IO_REPARSE_TAG_SYMLINK: u32 = 0xA000_000C;
    const SYMLINK_FLAG_RELATIVE: u32 = 1;
    const FILE_LIST_DIRECTORY: u32 = 0x1;
    const FILE_READ_ATTRIBUTES: u32 = 0x80;
    const FILE_WRITE_ATTRIBUTES: u32 = 0x100;
    const FILE_SYNCHRONOUS_IO_NONALERT: u32 = 0x20;

    fn last_error() -> u32 {
        // SAFETY: GetLastError only reads the calling thread's last-error value.
        unsafe { GetLastError() }
    }

    fn wide0(s: &OsStr) -> Vec<u16> {
        s.encode_wide().chain(std::iter::once(0)).collect()
    }

    /// A directory handle usable as `RootDirectory`.
    fn open_dir(path: &Path) -> OwnedHandle {
        let f = std::fs::OpenOptions::new()
            .access_mode(FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES | SYNCHRONIZE)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(path)
            .expect("open probe directory");
        OwnedHandle::from(f)
    }

    /// `NtCreateFile` relative to `dir`. Returns the status and, on success, the handle.
    fn nt_create(
        dir: &OwnedHandle,
        name: &str,
        access: u32,
        disposition: u32,
        options: u32,
    ) -> (i32, Option<OwnedHandle>) {
        let mut w: Vec<u16> = OsStr::new(name).encode_wide().collect();
        let bytes = (w.len() * 2) as u16;
        let us = UNICODE_STRING { Length: bytes, MaximumLength: bytes, Buffer: w.as_mut_ptr() };
        // SAFETY: OBJECT_ATTRIBUTES is plain data for which all-zero is a valid initial value.
        let mut oa: OBJECT_ATTRIBUTES = unsafe { std::mem::zeroed() };
        oa.Length = size_of::<OBJECT_ATTRIBUTES>() as u32;
        oa.RootDirectory = dir.as_raw_handle() as HANDLE;
        oa.ObjectName = &raw const us;
        let mut raw: HANDLE = std::ptr::null_mut();
        // SAFETY: IO_STATUS_BLOCK is plain data for which all-zero is valid.
        let mut iosb: IO_STATUS_BLOCK = unsafe { std::mem::zeroed() };
        // SAFETY: every pointer is to a live local; `w` outlives `us`.
        let status = unsafe {
            NtCreateFile(
                &raw mut raw,
                access,
                &raw const oa,
                &raw mut iosb,
                std::ptr::null(),
                0,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                disposition,
                options,
                std::ptr::null(),
                0,
            )
        };
        if status == 0 {
            // SAFETY: STATUS_SUCCESS, so `raw` is a handle we own.
            (status, Some(unsafe { OwnedHandle::from_raw_handle(raw as _) }))
        } else {
            (status, None)
        }
    }

    /// A symlink reparse buffer: header, then offsets/lengths, flags and the path buffer.
    fn symlink_buffer(target: &str, relative: bool) -> Vec<u8> {
        let print: Vec<u16> = OsStr::new(target).encode_wide().collect();
        // A relative payload keeps the same text for both names. An absolute one needs the NT form
        // `\??\` + the Win32 path as its substitute name; only the print name stays Win32.
        let substitute: Vec<u16> = if relative {
            print.clone()
        } else {
            OsStr::new("\\??\\").encode_wide().chain(print.iter().copied()).collect()
        };
        let sb = (substitute.len() * 2) as u16;
        let pb = (print.len() * 2) as u16;
        let mut b = Vec::new();
        b.extend_from_slice(&IO_REPARSE_TAG_SYMLINK.to_le_bytes());
        b.extend_from_slice(&(12 + sb + pb).to_le_bytes()); // ReparseDataLength
        b.extend_from_slice(&0u16.to_le_bytes()); // Reserved
        b.extend_from_slice(&0u16.to_le_bytes()); // SubstituteNameOffset
        b.extend_from_slice(&sb.to_le_bytes()); // SubstituteNameLength
        b.extend_from_slice(&sb.to_le_bytes()); // PrintNameOffset
        b.extend_from_slice(&pb.to_le_bytes()); // PrintNameLength
        b.extend_from_slice(&(if relative { SYMLINK_FLAG_RELATIVE } else { 0 }).to_le_bytes());
        for u in substitute.iter().chain(print.iter()) {
            b.extend_from_slice(&u.to_le_bytes());
        }
        b
    }

    /// `FSCTL_SET_REPARSE_POINT`; Ok or the `GetLastError` value.
    fn set_reparse(h: &OwnedHandle, buf: &[u8]) -> Result<(), u32> {
        let mut returned = 0u32;
        // SAFETY: `buf` and `returned` are live locals; the handle outlives the call.
        let ok = unsafe {
            DeviceIoControl(
                h.as_raw_handle() as HANDLE,
                FSCTL_SET_REPARSE_POINT,
                buf.as_ptr().cast(),
                buf.len() as u32,
                std::ptr::null_mut(),
                0,
                &raw mut returned,
                std::ptr::null_mut(),
            )
        };
        if ok != 0 { Ok(()) } else { Err(last_error()) }
    }

    fn create_symlink_w(link: &Path, target: &str, flags: u32) -> Result<(), u32> {
        let l = wide0(link.as_os_str());
        let t = wide0(OsStr::new(target));
        // SAFETY: both are NUL-terminated and live across the call.
        let ok = unsafe { CreateSymbolicLinkW(l.as_ptr(), t.as_ptr(), flags) };
        if ok { Ok(()) } else { Err(last_error()) }
    }

    fn show<E: std::fmt::Debug>(r: Result<(), E>) -> String {
        match r {
            Ok(()) => "Ok".to_string(),
            Err(e) => format!("Err({e:?})"),
        }
    }

    /// Rename `from` in `fd` to `to` in `td`. `ex = None`: classic `FileRenameInformation` with
    /// `ReplaceIfExists = true`. `ex = Some(flags)`: `FileRenameInformationEx` with those flags.
    /// Returns the failing or final stage (`open` of the source, or `set-information`) with its NTSTATUS.
    fn rename(
        fd: &OwnedHandle,
        from: &str,
        td: &OwnedHandle,
        to: &str,
        ex: Option<u32>,
    ) -> (&'static str, i32) {
        let (st, h) = nt_create(
            fd,
            from,
            DELETE | SYNCHRONIZE,
            FILE_OPEN,
            FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
        );
        let Some(h) = h else { return ("open", st) };
        let wide: Vec<u16> = OsStr::new(to).encode_wide().collect();
        let name_bytes = wide.len() * 2;
        let total = size_of::<FILE_RENAME_INFORMATION>() + name_bytes;
        let mut buf: Vec<u64> = vec![0u64; total.div_ceil(8)];
        let base: *mut u8 = buf.as_mut_ptr().cast();
        // SAFETY: IO_STATUS_BLOCK is plain data for which all-zero is valid.
        let mut iosb: IO_STATUS_BLOCK = unsafe { std::mem::zeroed() };
        // SAFETY: `base` addresses at least `total` zeroed, 8-aligned bytes; the name is written into
        // the trailing space; `buf` and `h` outlive the call.
        let st = unsafe {
            let info = base.cast::<FILE_RENAME_INFORMATION>();
            match ex {
                None => (&raw mut (*info).Anonymous.ReplaceIfExists).write(true),
                Some(flags) => (&raw mut (*info).Anonymous.Flags).write(flags),
            }
            (&raw mut (*info).RootDirectory).write(td.as_raw_handle() as HANDLE);
            (&raw mut (*info).FileNameLength).write(name_bytes as u32);
            std::ptr::copy_nonoverlapping(
                wide.as_ptr(),
                (&raw mut (*info).FileName).cast::<u16>(),
                wide.len(),
            );
            NtSetInformationFile(
                h.as_raw_handle() as _,
                &raw mut iosb,
                base.cast(),
                total as u32,
                if ex.is_some() { FileRenameInformationEx } else { FileRenameInformation },
            )
        };
        ("set-information", st)
    }

    /// The reparse tag of `path` itself (not followed), or a description of why it is absent.
    fn tag_of(path: &Path) -> String {
        let f = match std::fs::OpenOptions::new()
            .access_mode(FILE_READ_ATTRIBUTES)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)
        {
            Ok(f) => f,
            Err(e) => return format!("open failed: {e}"),
        };
        let h = OwnedHandle::from(f);
        #[repr(C)]
        struct Info {
            attrs: u32,
            tag: u32,
        }
        let mut info = Info { attrs: 0, tag: 0 };
        // SAFETY: IO_STATUS_BLOCK is plain data for which all-zero is valid.
        let mut iosb: IO_STATUS_BLOCK = unsafe { std::mem::zeroed() };
        // SAFETY: `info` is a live, correctly sized FILE_ATTRIBUTE_TAG_INFORMATION.
        let st = unsafe {
            NtQueryInformationFile(
                h.as_raw_handle() as _,
                &raw mut iosb,
                (&raw mut info).cast(),
                size_of::<Info>() as u32,
                FileAttributeTagInformation,
            )
        };
        if st != 0 {
            return format!("NTSTATUS=0x{:08X}", st as u32);
        }
        format!("attrs=0x{:X} tag=0x{:08X}", info.attrs, info.tag)
    }

    #[test]
    #[ignore = "owner-run measurement probe"]
    fn probe_create_symbolic_link_w_file_and_dir_and_dangling() {
        let td = tempfile::TempDir::new().unwrap();
        let p = td.path();
        std::fs::write(p.join("real-file"), b"x").unwrap();
        std::fs::create_dir(p.join("real-dir")).unwrap();
        let un = SYMBOLIC_LINK_FLAG_ALLOW_UNPRIVILEGED_CREATE;
        let cases: [(&str, &str, &str, u32); 4] = [
            ("file", "link-file", "real-file", un),
            ("dir", "link-dir", "real-dir", un | SYMBOLIC_LINK_FLAG_DIRECTORY),
            ("dangling-file-target", "link-dangling", "no-such-target", un),
            ("file-without-unprivileged-flag", "link-file-privileged", "real-file", 0),
        ];
        for (label, name, target, flags) in cases {
            let r = create_symlink_w(&p.join(name), target, flags);
            match r {
                Ok(()) => println!("PROBE create_symbolic_link_w {label}: Ok"),
                Err(e) => println!("PROBE create_symbolic_link_w {label}: GetLastError={e}"),
            }
        }
        println!("PROBE create_symbolic_link_w done (1314 = ERROR_PRIVILEGE_NOT_HELD)");
    }

    #[test]
    #[ignore = "owner-run measurement probe"]
    fn probe_nt_create_plus_fsctl_set_reparse_point() {
        let td = tempfile::TempDir::new().unwrap();
        let p = td.path();
        std::fs::create_dir(p.join("real-dir")).unwrap();
        let dir = open_dir(p);
        let access = FILE_GENERIC_WRITE | DELETE | SYNCHRONIZE | FILE_WRITE_ATTRIBUTES;
        for (label, name, is_dir, target, relative) in [
            ("file-relative", "nt-file-rel", false, "real-file", true),
            ("dir-relative", "nt-dir-rel", true, "real-dir", true),
            ("file-absolute", "nt-file-abs", false, "C:\\Windows\\notepad.exe", false),
        ] {
            let opts = FILE_OPEN_REPARSE_POINT
                | FILE_SYNCHRONOUS_IO_NONALERT
                | if is_dir { FILE_DIRECTORY_FILE } else { FILE_NON_DIRECTORY_FILE };
            let (st, h) = nt_create(&dir, name, access, FILE_CREATE, opts);
            println!("PROBE nt_create {label}: NTSTATUS=0x{:08X}", st as u32);
            let Some(h) = h else { continue };
            let r = set_reparse(&h, &symlink_buffer(target, relative));
            match r {
                Ok(()) => println!("PROBE fsctl_set_reparse_point {label}: Ok"),
                Err(e) => println!("PROBE fsctl_set_reparse_point {label}: Win32Error={e}"),
            }
            drop(h);
            match std::fs::read_link(p.join(name)) {
                Ok(t) => println!("PROBE read_link after nt-create {label}: {}", t.display()),
                Err(e) => println!("PROBE read_link after nt-create {label}: Err({e})"),
            }
        }
    }

    #[test]
    #[ignore = "owner-run measurement probe"]
    fn probe_rename_replace_over_and_with_a_directory_link() {
        let replace_posix = FILE_RENAME_REPLACE_IF_EXISTS | FILE_RENAME_POSIX_SEMANTICS;
        for (variant, ex) in [
            ("FileRenameInformation", None),
            ("FileRenameInformationEx+POSIX", Some(replace_posix)),
        ] {
            for (case, link_is_source) in
                [("dir-link-at-temp-over-file", true), ("file-at-temp-over-dir-link", false)]
            {
                let td = tempfile::TempDir::new().unwrap();
                let p = td.path();
                std::fs::create_dir(p.join("real-dir")).unwrap();
                std::fs::write(p.join("file"), b"x").unwrap();
                let made = create_symlink_w(
                    &p.join("link"),
                    "real-dir",
                    SYMBOLIC_LINK_FLAG_ALLOW_UNPRIVILEGED_CREATE | SYMBOLIC_LINK_FLAG_DIRECTORY,
                );
                if let Err(e) = made {
                    println!(
                        "PROBE rename {variant} {case}: SETUP link creation failed Win32Error={e}"
                    );
                    continue;
                }
                let dir = open_dir(p);
                let (stage, st) = if link_is_source {
                    rename(&dir, "link", &dir, "file", ex)
                } else {
                    rename(&dir, "file", &dir, "link", ex)
                };
                println!(
                    "PROBE rename {variant} {case}: stage={stage} NTSTATUS=0x{:08X}",
                    st as u32
                );
            }
            // Follow-ups for the owner: a directory link at the temp name over an existing
            // directory link, and a regular file at the temp name over a junction.
            for case in ["dir-link-at-temp-over-dir-link", "file-at-temp-over-junction"] {
                let td = tempfile::TempDir::new().unwrap();
                let p = td.path();
                std::fs::create_dir(p.join("real-dir")).unwrap();
                let flags =
                    SYMBOLIC_LINK_FLAG_ALLOW_UNPRIVILEGED_CREATE | SYMBOLIC_LINK_FLAG_DIRECTORY;
                if case == "dir-link-at-temp-over-dir-link" {
                    let made = create_symlink_w(&p.join("temp"), "real-dir", flags)
                        .and_then(|()| create_symlink_w(&p.join("link"), "real-dir", flags));
                    if let Err(e) = made {
                        println!(
                            "PROBE rename {variant} {case}: SETUP link creation failed Win32Error={e}"
                        );
                        continue;
                    }
                } else {
                    std::fs::write(p.join("temp"), b"x").unwrap();
                    let out = std::process::Command::new("cmd")
                        .arg("/C")
                        .arg("mklink")
                        .arg("/J")
                        .arg(OsString::from(p.join("link")))
                        .arg(OsString::from(p.join("real-dir")))
                        .output();
                    match out {
                        Ok(o) if o.status.success() => {}
                        Ok(o) => {
                            println!(
                                "PROBE rename {variant} {case}: SETUP junction failed status={}",
                                o.status
                            );
                            continue;
                        }
                        Err(e) => {
                            println!("PROBE rename {variant} {case}: SETUP spawn failed {e}");
                            continue;
                        }
                    }
                }
                let dir = open_dir(p);
                let (stage, st) = rename(&dir, "temp", &dir, "link", ex);
                println!(
                    "PROBE rename {variant} {case}: stage={stage} NTSTATUS=0x{:08X}",
                    st as u32
                );
            }
        }
    }

    #[test]
    #[ignore = "owner-run measurement probe"]
    fn probe_read_link_round_trip_and_junction_tag() {
        let td = tempfile::TempDir::new().unwrap();
        let p = td.path();
        std::fs::create_dir(p.join("sub")).unwrap();
        std::fs::write(p.join("sub").join("x"), b"x").unwrap();
        let un = SYMBOLIC_LINK_FLAG_ALLOW_UNPRIVILEGED_CREATE;
        println!("PROBE setup rel: {}", show(create_symlink_w(&p.join("rel"), "sub\\x", un)));
        println!(
            "PROBE setup abs: {}",
            show(create_symlink_w(
                &p.join("abs"),
                "C:\\Windows",
                un | SYMBOLIC_LINK_FLAG_DIRECTORY
            ))
        );
        let out = std::process::Command::new("cmd")
            .arg("/C")
            .arg("mklink")
            .arg("/J")
            .arg(OsString::from(p.join("junc")))
            .arg(OsString::from(p.join("sub")))
            .output();
        match out {
            Ok(o) => println!(
                "PROBE setup junction: status={} {}",
                o.status,
                String::from_utf8_lossy(&o.stdout).trim()
            ),
            Err(e) => println!("PROBE setup junction: spawn failed {e}"),
        }
        for name in ["rel", "abs", "junc"] {
            let path: PathBuf = p.join(name);
            match std::fs::read_link(&path) {
                Ok(t) => println!("PROBE read_link {name}: {}", t.display()),
                Err(e) => println!("PROBE read_link {name}: Err({e})"),
            }
            println!("PROBE reparse_tag {name}: {}", tag_of(&path));
        }
        println!("PROBE tags: 0xA000000C = symlink, 0xA0000003 = junction (mount point)");
        // A junction and a directory symlink must be removed as directories before the TempDir drops.
        for name in ["junc", "abs"] {
            let _ = std::fs::remove_dir(p.join(name));
        }
    }
}

#[cfg(target_os = "linux")]
mod linux {
    #[test]
    #[ignore = "owner-run measurement probe; needs FLUX_PROBE_DIR"]
    #[allow(non_snake_case)]
    fn probe_symlinkat_on_the_directory_named_by_FLUX_PROBE_DIR() {
        use rustix::fs::{AtFlags, CWD, Mode, OFlags, openat, symlinkat, unlinkat};
        let Some(dir) = std::env::var_os("FLUX_PROBE_DIR") else {
            println!("PROBE symlinkat: FLUX_PROBE_DIR is not set; nothing measured");
            return;
        };
        let fd = match openat(
            CWD,
            dir.as_os_str(),
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(fd) => fd,
            Err(e) => {
                println!("PROBE symlinkat: could not open {dir:?}: {e:?}");
                return;
            }
        };
        // A leftover from an earlier run would answer EEXIST and pollute the measurement.
        match unlinkat(&fd, "probe-link", AtFlags::empty()) {
            Ok(()) => println!("PROBE symlinkat: removed a leftover probe-link"),
            Err(e) if e == rustix::io::Errno::NOENT => {}
            Err(e) => println!("PROBE symlinkat: leftover removal failed: {e:?}"),
        }
        match rustix::fs::fstatfs(&fd) {
            Ok(sf) => println!("PROBE symlinkat {dir:?}: statfs f_type=0x{:X}", sf.f_type as u64),
            Err(e) => println!("PROBE symlinkat {dir:?}: statfs failed {e:?}"),
        }
        match symlinkat("target", &fd, "probe-link") {
            Ok(()) => {
                println!("PROBE symlinkat {dir:?}: Ok");
                if let Err(e) = unlinkat(&fd, "probe-link", AtFlags::empty()) {
                    println!("PROBE symlinkat cleanup failed: {e:?}");
                }
            }
            Err(e) => {
                println!("PROBE symlinkat {dir:?}: Err {e:?} (raw os error {})", e.raw_os_error())
            }
        }
    }
}
