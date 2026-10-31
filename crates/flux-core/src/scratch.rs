// Copyright (c) 2026 Costas Kirgoussios
//
// This software is licensed under the PolyForm Noncommercial License 1.0.0.
// You may obtain a copy of the License at:
// https://polyformproject.org/licenses/noncommercial/1.0.0/

//! The scratch space a bounded listing spills into: one anonymous file in a directory, carved into regions that
//! are truncated in stack order.

use flux_fs::{Code, DestinationRoot, DirHandle, FsError};
use std::cell::{Cell, RefCell};
use std::fs::File;
use std::path::{Path, PathBuf};

/// The sizes that decide when a directory listing spills and how its runs are merged (spec 6.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Thresholds {
    /// A directory of at most this many entries is sorted in memory.
    pub inline_entries: usize,
    /// Entries per sorted run written to the scratch file.
    pub run_entries: usize,
    /// Runs merged at once.
    pub merge_fan_in: usize,
    /// Bytes of read buffer per run during a merge.
    pub merge_buffer: usize,
    /// Entries between two ticks (heartbeat and cancellation checks).
    pub tick_entries: usize,
}

impl Default for Thresholds {
    fn default() -> Self {
        Thresholds {
            inline_entries: 8_192,
            run_entries: 262_144,
            merge_fan_in: 384,
            merge_buffer: 16_384,
            tick_entries: 65_536,
        }
    }
}

impl Thresholds {
    /// Refuses a combination the spill cannot work with.
    pub fn new(
        inline_entries: usize,
        run_entries: usize,
        merge_fan_in: usize,
        merge_buffer: usize,
        tick_entries: usize,
    ) -> Result<Thresholds, &'static str> {
        if run_entries == 0 {
            return Err("run_entries must be at least 1");
        }
        if inline_entries > run_entries {
            return Err("inline_entries must not exceed run_entries");
        }
        if merge_fan_in < 2 {
            return Err("merge_fan_in must be at least 2");
        }
        if merge_buffer < 4 {
            return Err("merge_buffer must be at least 4");
        }
        Ok(Thresholds { inline_entries, run_entries, merge_fan_in, merge_buffer, tick_entries })
    }
}

/// How the home creates its file, erased: `DirHandle` has associated types and cannot be a trait object.
type Create<'h> = Box<dyn Fn() -> flux_fs::Result<File> + 'h>;

/// Finds the home when the first directory spills, not before: a run that never spills touches nothing (spec 6.1).
type Deferred<'h> = Box<dyn FnOnce() -> Result<(Create<'h>, PathBuf), FsError> + 'h>;

enum Home<'h> {
    Unplaced,
    /// Placed, but the location is not looked at (let alone made) until the first `begin`.
    Deferred(Deferred<'h>),
    Placed {
        create: Create<'h>,
        shown: PathBuf,
    },
    /// The home could not be found; returned by every `begin`.
    Failed {
        code: Code,
        message: String,
    },
}

enum Backing {
    NotCreated,
    Open(File),
    /// Creation failed; kept and returned on every attempt, as `HardlinkSlot` keeps its own.
    Failed {
        code: Code,
        message: String,
    },
    Closed,
}

fn fatal(message: impl Into<String>) -> FsError {
    FsError::new(Code::IoError, std::io::Error::other(message.into()))
}

/// One anonymous file in a directory, carved into regions that are truncated in stack order.
///
/// INVARIANT: one ACTIVE walk per space. Regions are LIFO, so two walks iterated in alternation on one space (as
/// `verify` iterates its two sides) would truncate each other's runs. `begin` from a second walk while the first
/// still holds a region is therefore a fatal error, not silent corruption. Each walk takes its owner token from
/// `ScratchSpace::next_owner` when it is built.
pub struct ScratchSpace<'h> {
    thresholds: Thresholds,
    explicit: Option<PathBuf>,
    home: RefCell<Home<'h>>,
    backing: RefCell<Backing>,
    /// The logical end of the file.
    end: Cell<u64>,
    poisoned: Cell<bool>,
    live_regions: Cell<usize>,
    holder: Cell<u64>,
    owners: Cell<u64>,
    #[cfg(test)]
    writes_before_failure: Cell<Option<u64>>,
    #[cfg(test)]
    truncate_fails: Cell<bool>,
    #[cfg(test)]
    reads_before_failure: Cell<Option<u64>>,
}

impl<'h> ScratchSpace<'h> {
    /// A space with no home yet. `explicit` is the operator's `--scratch-dir`, carried here so every placement site
    /// sees it. A spill while unplaced is a fatal error (not a panic: a release build aborts on panic).
    pub fn unplaced(thresholds: Thresholds, explicit: Option<PathBuf>) -> ScratchSpace<'h> {
        ScratchSpace {
            thresholds,
            explicit,
            home: RefCell::new(Home::Unplaced),
            backing: RefCell::new(Backing::NotCreated),
            end: Cell::new(0),
            poisoned: Cell::new(false),
            live_regions: Cell::new(0),
            holder: Cell::new(0),
            owners: Cell::new(0),
            #[cfg(test)]
            writes_before_failure: Cell::new(None),
            #[cfg(test)]
            truncate_fails: Cell::new(false),
            #[cfg(test)]
            reads_before_failure: Cell::new(None),
        }
    }

    pub fn explicit(&self) -> Option<&Path> {
        self.explicit.as_deref()
    }

    pub fn thresholds(&self) -> Thresholds {
        self.thresholds
    }

    /// A fresh owner token for a walk.
    pub(crate) fn next_owner(&self) -> u64 {
        let n = self.owners.get() + 1;
        self.owners.set(n);
        n
    }

    /// Places the home lazily: `make` runs once, at the first `begin`, so nothing is looked at or created for a run
    /// that never spills. Its `Err` is remembered and returned by every `begin`. A second placement is ignored.
    pub fn place_deferred<D, G>(&self, make: G)
    where
        D: DirHandle + 'h,
        G: FnOnce() -> Result<(D, PathBuf), FsError> + 'h,
    {
        let mut slot = self.home.borrow_mut();
        if matches!(*slot, Home::Unplaced) {
            *slot = Home::Deferred(Box::new(move || {
                let (dir, shown) = make()?;
                let create: Create<'h> = Box::new(move || dir.create_scratch());
                Ok((create, shown))
            }));
        }
    }

    /// Sets the home once (a second call is ignored). Nothing is created until the first write.
    pub fn place<D: DirHandle + 'h>(&self, home: D, shown: PathBuf) {
        self.place_with(Box::new(move || home.create_scratch()), shown);
    }

    fn place_with(&self, create: Create<'h>, shown: PathBuf) {
        let mut slot = self.home.borrow_mut();
        if matches!(*slot, Home::Unplaced) {
            *slot = Home::Placed { create, shown };
        }
    }

    /// A region starting at the file's current length. Creates the file on the first call.
    pub(crate) fn begin(&self, owner: u64) -> Result<Region<'_, 'h>, FsError> {
        self.check_usable()?;
        if self.live_regions.get() > 0 && self.holder.get() != owner {
            return Err(fatal("one scratch space is shared by two walks"));
        }
        self.ensure_open()?;
        self.holder.set(owner);
        self.live_regions.set(self.live_regions.get() + 1);
        Ok(Region { space: self, start: self.end.get() })
    }

    pub(crate) fn write_at(&self, offset: u64, bytes: &[u8]) -> Result<(), FsError> {
        self.check_usable()?;
        #[cfg(test)]
        if let Some(n) = self.writes_before_failure.get() {
            if n == 0 {
                return Err(FsError::new(
                    Code::DiskFull,
                    std::io::Error::other("injected scratch write failure"),
                ));
            }
            self.writes_before_failure.set(Some(n - 1));
        }
        let backing = self.backing.borrow();
        let Backing::Open(file) = &*backing else {
            return Err(self.not_open(&backing));
        };
        write_all_at(file, bytes, offset).map_err(|e| self.io_error(e))?;
        let new_end = offset + bytes.len() as u64;
        if new_end > self.end.get() {
            self.end.set(new_end);
        }
        Ok(())
    }

    pub(crate) fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, FsError> {
        self.check_usable()?;
        #[cfg(test)]
        if let Some(n) = self.reads_before_failure.get() {
            if n == 0 {
                return Err(FsError::new(
                    Code::IoError,
                    std::io::Error::other("injected scratch read failure"),
                ));
            }
            self.reads_before_failure.set(Some(n - 1));
        }
        let backing = self.backing.borrow();
        let Backing::Open(file) = &*backing else {
            return Err(self.not_open(&backing));
        };
        read_fully_at(file, buf, offset).map_err(|e| self.io_error(e))
    }

    /// The logical end of the file.
    pub(crate) fn len(&self) -> u64 {
        self.end.get()
    }

    // Read only by tests so far: every scratch call checks the flag itself.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn poisoned(&self) -> bool {
        self.poisoned.get()
    }

    /// Drops the file (on Windows the delete-on-close handle, without which the workspace cannot be removed). A
    /// later `begin` is a fatal error.
    pub fn close(&self) {
        *self.backing.borrow_mut() = Backing::Closed;
        // The home holds the workspace's directory handle (inside the create closure): release it too, or the
        // workspace cannot be removed while the space lives. Placed after the old home is dropped.
        let old = std::mem::replace(
            &mut *self.home.borrow_mut(),
            Home::Failed { code: Code::IoError, message: String::from("scratch space is closed") },
        );
        drop(old);
    }

    /// The file's real length, as the operating system reports it (not the logical end).
    #[cfg(test)]
    pub(crate) fn physical_len(&self) -> u64 {
        match &*self.backing.borrow() {
            Backing::Open(file) => file.metadata().unwrap().len(),
            _ => panic!("the scratch file is not open"),
        }
    }

    /// The `n+1`-th `write_at` fails with `Code::DiskFull`.
    #[cfg(test)]
    pub(crate) fn fail_writes_after(&self, n: u64) {
        self.writes_before_failure.set(Some(n));
    }

    /// The `n+1`-th `read_at` fails with `Code::IoError`.
    #[cfg(test)]
    pub(crate) fn fail_reads_after(&self, n: u64) {
        self.reads_before_failure.set(Some(n));
    }

    /// The next truncate fails.
    #[cfg(test)]
    fn fail_truncate(&self) {
        self.truncate_fails.set(true);
    }

    fn check_usable(&self) -> Result<(), FsError> {
        if self.poisoned.get() {
            return Err(fatal("scratch space is unusable after a failed truncate"));
        }
        Ok(())
    }

    fn shown(&self) -> String {
        match &*self.home.borrow() {
            Home::Placed { shown, .. } => shown.display().to_string(),
            _ => String::from("scratch space"),
        }
    }

    /// An I/O failure on the file, classified and naming the location; the code is taken before the message is
    /// rebuilt, so `ENOSPC` stays `DiskFull`.
    fn io_error(&self, e: std::io::Error) -> FsError {
        // `.source` is deliberately rebuilt to name `shown`; consumers must use `.code`, not `raw_os_error`.
        let code = FsError::classify(&e);
        FsError::new(code, std::io::Error::new(e.kind(), format!("{}: {e}", self.shown())))
    }

    fn not_open(&self, backing: &Backing) -> FsError {
        match backing {
            Backing::Closed => fatal("scratch space is closed"),
            Backing::Failed { code, message } => {
                FsError::new(*code, std::io::Error::other(message.clone()))
            }
            _ => fatal("scratch space has no file yet"),
        }
    }

    fn ensure_open(&self) -> Result<(), FsError> {
        let mut backing = self.backing.borrow_mut();
        if matches!(*backing, Backing::NotCreated) {
            self.resolve_home();
            *backing = match &*self.home.borrow() {
                Home::Deferred(_) | Home::Unplaced => {
                    return Err(fatal("scratch space used before it was placed"));
                }
                Home::Failed { code, message } => {
                    return Err(FsError::new(*code, std::io::Error::other(message.clone())));
                }
                Home::Placed { create, shown } => match create() {
                    Ok(file) => Backing::Open(file),
                    Err(e) => Backing::Failed {
                        code: e.code,
                        message: format!("{}: {}", shown.display(), e.source),
                    },
                },
            };
        }
        match &*backing {
            Backing::Open(_) => Ok(()),
            other => Err(self.not_open(other)),
        }
    }

    /// Runs a deferred home's producer (once); a failure becomes the remembered `Home::Failed`.
    fn resolve_home(&self) {
        let deferred = {
            let mut home = self.home.borrow_mut();
            if !matches!(*home, Home::Deferred(_)) {
                return;
            }
            std::mem::replace(&mut *home, Home::Unplaced)
        };
        let Home::Deferred(make) = deferred else { return };
        let resolved = match make() {
            Ok((create, shown)) => Home::Placed { create, shown },
            Err(e) => Home::Failed { code: e.code, message: e.source.to_string() },
        };
        *self.home.borrow_mut() = resolved;
    }

    fn truncate(&self, start: u64) {
        #[cfg(test)]
        if self.truncate_fails.replace(false) {
            self.poisoned.set(true);
            return;
        }
        if let Backing::Open(file) = &*self.backing.borrow()
            && file.set_len(start).is_err()
        {
            self.poisoned.set(true);
        }
    }
}

/// A span of the file from `start` to its end at the time it is dropped; dropping truncates the file back to
/// `start`. Regions nest like a stack.
pub(crate) struct Region<'s, 'h> {
    space: &'s ScratchSpace<'h>,
    start: u64,
}

impl Region<'_, '_> {
    // Read only by tests: the walk never needs a region's start.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn start(&self) -> u64 {
        self.start
    }
}

impl Drop for Region<'_, '_> {
    fn drop(&mut self) {
        // A region dropped after its parent finds the end already below its start: it must neither extend the file
        // (`set_len` past the end pads with zeros) nor raise the end. At `start == end` the truncate still runs,
        // because a failed partial write can leave bytes physically past `end`.
        if self.start <= self.space.end.get() {
            self.space.truncate(self.start);
            self.space.end.set(self.start);
        }
        self.space.live_regions.set(self.space.live_regions.get().saturating_sub(1));
    }
}

impl ScratchSpace<'_> {
    /// Whether a home (or a remembered failure to find one) was set.
    pub(crate) fn is_placed(&self) -> bool {
        !matches!(*self.home.borrow(), Home::Unplaced)
    }
}

const NO_HOME: &str = "no disk-backed scratch location (the temporary directory is memory-backed); pass --scratch-dir PATH";

/// Where a run without a placed workspace (a dry run, the lockless library copy) keeps its scratch file (spec 6.1):
/// `explicit` wins with no memory check; else `temp_root` unless it is memory-backed; else the user cache directory.
pub(crate) fn temporary_home<F: DestinationRoot>(
    fs: &F,
    explicit: Option<&Path>,
    temp_root: &Path,
) -> Result<(F::Dir, PathBuf), FsError> {
    let caches = cache_candidates();
    temporary_home_from(fs, explicit, temp_root, &caches)
}

/// The user cache directories to try, from the environment.
pub(crate) fn cache_candidates() -> Vec<PathBuf> {
    cache_candidates_from(
        std::env::var_os("XDG_CACHE_HOME"),
        std::env::var_os("HOME"),
        std::env::var_os("LOCALAPPDATA"),
    )
}

/// A relative or empty value is ignored (the XDG rule, applied to all three).
fn cache_candidates_from(
    xdg: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
    local_app_data: Option<std::ffi::OsString>,
) -> Vec<PathBuf> {
    let absolute = |v: Option<std::ffi::OsString>| v.map(PathBuf::from).filter(|p| p.is_absolute());
    if cfg!(windows) {
        absolute(local_app_data).into_iter().collect()
    } else {
        absolute(xdg).into_iter().chain(absolute(home).map(|h| h.join(".cache"))).collect()
    }
}

fn temporary_home_from<F: DestinationRoot>(
    fs: &F,
    explicit: Option<&Path>,
    temp_root: &Path,
    caches: &[PathBuf],
) -> Result<(F::Dir, PathBuf), FsError> {
    if let Some(path) = explicit {
        return Ok((fs.destination_root(path)?, path.to_path_buf()));
    }
    // A temporary directory that cannot be opened is not "memory-backed": its own error is the answer, and nothing
    // (no cache directory) is created for a run that may never spill.
    let dir = fs.destination_root(temp_root)?;
    if dir.memory_backed().unwrap_or(None) != Some(true) {
        return Ok(windows_subdir(dir, temp_root.to_path_buf()));
    }
    for cache in caches {
        let dir = match fs.destination_root(cache) {
            Ok(dir) => dir,
            Err(e) if e.source.kind() == std::io::ErrorKind::NotFound => {
                match fs.create_dir(cache).and_then(|()| fs.destination_root(cache)) {
                    Ok(dir) => dir,
                    Err(_) => continue,
                }
            }
            Err(_) => continue,
        };
        if dir.memory_backed().unwrap_or(None) != Some(true) {
            return Ok(windows_subdir(dir, cache.clone()));
        }
    }
    Err(fatal(NO_HOME))
}

/// On Windows the scratch file lives in a `flux-scratch` subdirectory, whose files older than 24 hours (a crash's
/// leftovers) are removed on first use. Best effort: any error keeps the directory itself.
#[cfg(windows)]
fn windows_subdir<D: DirHandle>(dir: D, shown: PathBuf) -> (D, PathBuf) {
    let name = std::ffi::OsStr::new("flux-scratch");
    let sub = match dir.create_dir(name) {
        Ok(sub) => sub,
        Err(e) if e.source.kind() == std::io::ErrorKind::AlreadyExists => {
            match dir.open_dir(name) {
                Ok(sub) => sub,
                Err(_) => return (dir, shown),
            }
        }
        Err(_) => return (dir, shown),
    };
    let day = std::time::Duration::from_secs(24 * 60 * 60);
    if let Ok(entries) = sub.read_dir() {
        for entry in entries {
            if entry.file_type == flux_fs::FileType::File
                && let Ok(m) = sub.metadata(&entry.name)
                && let Some(age) = m.modified.and_then(|t| t.elapsed().ok())
                && age > day
            {
                let _ignored = sub.remove_file(&entry.name);
            }
        }
    }
    (sub, shown.join("flux-scratch"))
}

#[cfg(not(windows))]
fn windows_subdir<D: DirHandle>(dir: D, shown: PathBuf) -> (D, PathBuf) {
    (dir, shown)
}

#[cfg(unix)]
fn write_all_at(file: &File, bytes: &[u8], offset: u64) -> std::io::Result<()> {
    std::os::unix::fs::FileExt::write_all_at(file, bytes, offset)
}

#[cfg(windows)]
fn write_all_at(file: &File, mut bytes: &[u8], mut offset: u64) -> std::io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !bytes.is_empty() {
        let n = file.seek_write(bytes, offset)?;
        if n == 0 {
            return Err(std::io::ErrorKind::WriteZero.into());
        }
        bytes = &bytes[n..];
        offset += n as u64;
    }
    Ok(())
}

/// Reads until `buf` is full or the file ends; returns the bytes read.
fn read_fully_at(file: &File, buf: &mut [u8], offset: u64) -> std::io::Result<usize> {
    let mut done = 0;
    while done < buf.len() {
        #[cfg(unix)]
        let n = std::os::unix::fs::FileExt::read_at(file, &mut buf[done..], offset + done as u64);
        #[cfg(windows)]
        let n =
            std::os::windows::fs::FileExt::seek_read(file, &mut buf[done..], offset + done as u64);
        match n {
            Ok(0) => break,
            Ok(n) => done += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fault_fs::FaultFs;
    use flux_fs::{Code, DestinationRoot, FileSystem, FsError};
    use std::path::Path;

    fn placed(fs: &FaultFs) -> ScratchSpace<'static> {
        fs.create_dir(Path::new("/s")).unwrap();
        let home = fs.destination_root(Path::new("/s")).unwrap();
        let space = ScratchSpace::unplaced(Thresholds::default(), None);
        space.place(home, PathBuf::from("/s"));
        space
    }

    /// The file's real length, as the operating system reports it (not the logical end).
    fn physical_len(space: &ScratchSpace<'_>) -> u64 {
        space.physical_len()
    }

    fn creations(fs: &FaultFs) -> usize {
        fs.calls().iter().filter(|c| c.starts_with("create_scratch(")).count()
    }

    #[test]
    fn thresholds_refuse_an_inline_limit_above_the_run_size() {
        assert!(Thresholds::new(9, 8, 2, 4, 1).is_err());
        assert!(Thresholds::new(8, 8, 2, 4, 1).is_ok());
        assert!(Thresholds::new(1, 0, 2, 4, 1).is_err());
        assert!(Thresholds::new(1, 8, 1, 4, 1).is_err());
        assert!(Thresholds::new(1, 8, 2, 3, 1).is_err());
        let d = Thresholds::default();
        assert_eq!(
            Thresholds::new(
                d.inline_entries,
                d.run_entries,
                d.merge_fan_in,
                d.merge_buffer,
                d.tick_entries
            ),
            Ok(d)
        );
    }

    #[test]
    fn nothing_is_created_until_the_first_region() {
        let fs = FaultFs::new();
        let space = placed(&fs);
        assert_eq!(creations(&fs), 0, "{:?}", fs.calls());
        let _r = space.begin(1).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(creations(&fs), 1, "{:?}", fs.calls());
    }

    #[test]
    fn an_unplaced_space_refuses_to_spill() {
        let space = ScratchSpace::unplaced(Thresholds::default(), None);
        let err = space.begin(1).err().expect("refused");
        assert!(err.to_string().contains("before it was placed"), "{err}");
    }

    #[test]
    fn nested_regions_truncate_in_lifo_order() {
        let fs = FaultFs::new();
        let space = placed(&fs);
        let a = space.begin(1).ok().unwrap();
        space.write_at(a.start(), &[1u8; 10]).unwrap();
        {
            let b = space.begin(1).ok().unwrap();
            assert_eq!(b.start(), 10);
            space.write_at(b.start(), &[2u8; 5]).unwrap();
            assert_eq!(space.len(), 15);
            assert_eq!(physical_len(&space), 15);
        }
        assert_eq!(space.len(), 10);
        assert_eq!(physical_len(&space), 10);
        let mut buf = [0u8; 10];
        assert_eq!(space.read_at(0, &mut buf).unwrap(), 10);
        assert_eq!(buf, [1u8; 10]);
        drop(a);
        assert_eq!(space.len(), 0);
        assert_eq!(physical_len(&space), 0);
    }

    #[test]
    fn a_parent_dropped_before_its_child_leaves_the_file_empty() {
        let fs = FaultFs::new();
        let space = placed(&fs);
        let a = space.begin(1).ok().unwrap();
        space.write_at(a.start(), &[1u8; 10]).unwrap();
        let b = space.begin(1).ok().unwrap();
        space.write_at(b.start(), &[2u8; 5]).unwrap();
        drop(a);
        assert_eq!((space.len(), physical_len(&space)), (0, 0));
        drop(b);
        assert_eq!((space.len(), physical_len(&space)), (0, 0));
        assert!(space.begin(1).is_ok(), "the space is still usable");
    }

    #[test]
    fn an_injected_write_failure_is_returned_and_the_file_ends_empty() {
        let fs = FaultFs::new();
        let space = placed(&fs);
        space.fail_writes_after(1);
        {
            let r = space.begin(1).ok().unwrap();
            space.write_at(r.start(), b"abc").unwrap();
            let err = space.write_at(r.start() + 3, b"def").unwrap_err();
            assert_eq!(err.code, Code::DiskFull);
            // A partial write that failed halfway leaves bytes physically past the logical end.
            if let Backing::Open(file) = &*space.backing.borrow() {
                write_all_at(file, b"de", r.start() + 3).unwrap();
            }
            assert_eq!((space.len(), physical_len(&space)), (3, 5));
        }
        assert_eq!(space.len(), 0);
        assert_eq!(physical_len(&space), 0);
    }

    #[test]
    fn a_second_walk_cannot_begin_while_the_first_holds_a_region() {
        let fs = FaultFs::new();
        let space = placed(&fs);
        let first = space.begin(1).ok().unwrap();
        let err = space.begin(2).err().expect("refused");
        assert!(err.to_string().contains("shared by two walks"), "{err}");
        drop(first);
        assert!(space.begin(2).is_ok());
    }

    #[test]
    fn a_closed_space_refuses_to_spill() {
        let fs = FaultFs::new();
        let space = placed(&fs);
        drop(space.begin(1).ok().unwrap());
        space.close();
        assert!(space.begin(1).is_err());
    }

    #[test]
    fn a_failed_creation_is_returned_on_every_attempt() {
        let fs = FaultFs::new();
        let space = placed(&fs);
        fs.fail("create_scratch", Code::PermissionDenied);
        for _ in 0..2 {
            let err = space.begin(1).err().expect("refused");
            assert_eq!(err.code, Code::PermissionDenied);
        }
        assert_eq!(creations(&fs), 1, "{:?}", fs.calls());
    }

    #[test]
    fn a_failed_truncate_poisons_every_later_call() {
        let fs = FaultFs::new();
        let space = placed(&fs);
        {
            let r = space.begin(1).ok().unwrap();
            space.write_at(r.start(), b"abc").unwrap();
            space.fail_truncate();
        }
        assert!(space.poisoned());
        let want = "unusable after a failed truncate";
        assert!(space.begin(1).err().unwrap().to_string().contains(want));
        assert!(space.write_at(0, b"x").unwrap_err().to_string().contains(want));
        assert!(space.read_at(0, &mut [0u8; 1]).unwrap_err().to_string().contains(want));
    }

    #[test]
    fn a_place_failure_is_remembered_until_a_spill_needs_it() {
        let space = ScratchSpace::unplaced(Thresholds::default(), None);
        let home: Result<(crate::fault_fs::FakeDirHandle, PathBuf), FsError> =
            Err(FsError::new(Code::PermissionDenied, std::io::Error::other("no temp dir")));
        space.place_deferred(move || home);
        let err = space.begin(1).err().expect("refused");
        assert_eq!(err.code, Code::PermissionDenied);
        // Remembered, not retried: every later attempt answers the same.
        assert_eq!(space.begin(1).err().expect("refused again").code, Code::PermissionDenied);
    }

    #[test]
    fn a_deferred_home_is_asked_once_and_only_at_the_first_begin() {
        use std::rc::Rc;
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/s")).unwrap();
        let asked = Rc::new(Cell::new(0));
        let space = ScratchSpace::unplaced(Thresholds::default(), None);
        let counter = Rc::clone(&asked);
        let fs_ref = &fs;
        space.place_deferred(move || {
            counter.set(counter.get() + 1);
            Ok((fs_ref.destination_root(Path::new("/s"))?, PathBuf::from("/s")))
        });
        assert!(space.is_placed());
        assert_eq!(asked.get(), 0, "placing asks nothing");
        let Ok(first) = space.begin(1) else { panic!("the home resolves") };
        assert_eq!(asked.get(), 1);
        drop(first);
        drop(space.begin(1).ok().unwrap());
        assert_eq!(asked.get(), 1, "asked once");
    }

    /// Drops a flag when the closure that holds the home's handle is dropped.
    struct Held(std::rc::Rc<Cell<bool>>);
    impl Drop for Held {
        fn drop(&mut self) {
            self.0.set(true);
        }
    }

    #[test]
    fn close_releases_the_home_and_a_later_begin_is_the_closed_error() {
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/s")).unwrap();
        let dir = fs.destination_root(Path::new("/s")).unwrap();
        let released = std::rc::Rc::new(Cell::new(false));
        let held = Held(std::rc::Rc::clone(&released));
        let space = ScratchSpace::unplaced(Thresholds::default(), None);
        space.place_with(
            Box::new(move || {
                let _held = &held;
                dir.create_scratch()
            }),
            PathBuf::from("/s"),
        );
        space.begin(1).map(drop).unwrap_or_else(|e| panic!("opens: {e}"));
        assert!(!released.get(), "the home is held while the space is open");
        space.close();
        assert!(released.get(), "close drops what holds the workspace handle");
        let err = space.begin(1).err().expect("refused");
        assert!(err.to_string().contains("closed"), "{err}");
    }

    #[test]
    fn close_before_any_spill_releases_an_unresolved_home_too() {
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/s")).unwrap();
        let space = ScratchSpace::unplaced(Thresholds::default(), None);
        let fs_ref = &fs;
        let asked = std::rc::Rc::new(Cell::new(false));
        let flag = std::rc::Rc::clone(&asked);
        space.place_deferred(move || {
            flag.set(true);
            Ok((fs_ref.destination_root(Path::new("/s"))?, PathBuf::from("/s")))
        });
        space.close();
        assert!(!asked.get(), "a closed space never resolves its home");
        assert!(space.begin(1).err().expect("refused").to_string().contains("closed"));
    }

    #[cfg(unix)]
    #[test]
    fn a_relative_or_empty_environment_value_is_not_a_cache_directory() {
        use std::ffi::OsString;
        let os = |s: &str| Some(OsString::from(s));
        assert_eq!(
            cache_candidates_from(os("/x/cache"), os("/home/u"), None),
            [PathBuf::from("/x/cache"), PathBuf::from("/home/u/.cache")]
        );
        assert_eq!(
            cache_candidates_from(os("rel/cache"), os("/home/u"), None),
            [PathBuf::from("/home/u/.cache")]
        );
        assert_eq!(cache_candidates_from(os(""), os(""), None), Vec::<PathBuf>::new());
        assert_eq!(cache_candidates_from(None, os("home"), None), Vec::<PathBuf>::new());
    }
    fn home_fs() -> FaultFs {
        use flux_fs::FileSystem;
        let fs = FaultFs::new();
        for d in ["/tmp", "/cache", "/mine"] {
            fs.create_dir(Path::new(d)).unwrap();
        }
        fs
    }

    #[test]
    fn temporary_home_prefers_explicit_then_a_disk_backed_temp_root_then_the_cache() {
        let fs = home_fs();
        let caches = [PathBuf::from("/cache"), PathBuf::from("/absent/.cache")];
        fs.set_memory_backed("/mine", Some(true));
        let (_, shown) =
            temporary_home_from(&fs, Some(Path::new("/mine")), Path::new("/tmp"), &caches).unwrap();
        assert_eq!(shown, Path::new("/mine"), "explicit wins with no memory check");
        let (_, shown) = temporary_home_from(&fs, None, Path::new("/tmp"), &caches).unwrap();
        assert_eq!(shown, Path::new("/tmp"));
        fs.set_memory_backed("/tmp", Some(true));
        let (_, shown) = temporary_home_from(&fs, None, Path::new("/tmp"), &caches).unwrap();
        assert_eq!(shown, Path::new("/cache"));
        // An unknown answer is not "memory-backed".
        fs.set_memory_backed("/tmp", None);
        let (_, shown) = temporary_home_from(&fs, None, Path::new("/tmp"), &caches).unwrap();
        assert_eq!(shown, Path::new("/tmp"));
    }

    #[test]
    fn temporary_home_makes_an_absent_cache_and_names_the_flag_when_nothing_fits() {
        let fs = home_fs();
        fs.set_memory_backed("/tmp", Some(true));
        let caches = [PathBuf::from("/cache/sub")];
        let (_, shown) = temporary_home_from(&fs, None, Path::new("/tmp"), &caches).unwrap();
        assert_eq!(shown, Path::new("/cache/sub"));
        assert!(fs.exists("/cache/sub"), "made because it was absent");
        fs.set_memory_backed("/cache/sub", Some(true));
        let Err(e) = temporary_home_from(&fs, None, Path::new("/tmp"), &caches) else {
            panic!("every location is memory-backed");
        };
        assert_eq!(e.code, Code::IoError);
        assert_eq!(
            e.source.to_string(),
            "no disk-backed scratch location (the temporary directory is memory-backed); pass --scratch-dir PATH"
        );
    }
}
