//! A filesystem that does what you tell it and remembers what you asked.
//!
//! The spec's rules are about ORDER — metadata before publication, no publication
//! after a strict failure — so this records the call sequence and can fail any
//! named call. None of that is reachable against a real disk.

use flux_fs::{Code, DirEntry, FileHandle, FileSystem, FileType, FsError, Metadata, Perms, Result};
use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

#[derive(Default)]
struct Inner {
    files: HashMap<PathBuf, Vec<u8>>,
    /// The ORDER of calls, which is what every assertion on it checks. Formatted with
    /// the lossy `display()` deliberately: this is an order oracle, not a path oracle,
    /// and no test distinguishes two paths by this string. The MAPS above are keyed by
    /// `PathBuf` because a lossy key there would merge two distinct objects, which is
    /// a correctness bug; a lossy LOG is only ever a less precise assertion message.
    /// If a test ever needs to tell two non-UTF-8 names apart HERE, add a lossless
    /// accessor then -- it is not needed now.
    calls: Vec<String>,
    faults: HashMap<String, Code>,
    times: HashMap<PathBuf, Option<SystemTime>>,
    /// path -> bytes appended on the second `metadata` call
    grow: HashMap<PathBuf, Vec<u8>>,
    metadata_reads: HashMap<PathBuf, u32>,
    /// call name -> code, not consumed on use
    always: HashMap<String, Code>,
    /// paths removed on the second `metadata` call
    vanish: HashSet<PathBuf>,
    /// path -> what `metadata` reports it as. One map, not three overlapping sets:
    /// `directories`, `symlinks` and `not_files` as separate sets could put one path
    /// in two of them at once and express a state no filesystem has. A path absent
    /// from this map but present in `files` is a regular file.
    types: HashMap<PathBuf, FileType>,
    /// Directories created via `create_dir`. Separate from `types`/`files`: a
    /// directory has no bytes (so it cannot live in `files`) and an empty
    /// directory must be distinguishable from a device node (`types` alone
    /// could not tell them apart, since neither is in `files`).
    directories: HashSet<PathBuf>,
    perms: HashMap<PathBuf, Option<Perms>>,
    /// consumed by the next `write` on a handle from `create_new`
    write_fault: Option<std::io::Error>,
    /// call name -> the `ErrorKind` an injected fault should carry. Without this the
    /// fake could only ever produce `ErrorKind::Other`, so code branching on the kind
    /// was unreachable from a test -- `cargo mutants` measured exactly that, and two
    /// `NotFound` guards survived mutation because of it.
    fault_kinds: HashMap<String, std::io::ErrorKind>,
    /// call name -> (which call, code, kind), for a step that calls the same method
    /// more than once and needs the LATER one to fail.
    nth_faults: HashMap<String, (u32, Code, std::io::ErrorKind)>,
    call_counts: HashMap<String, u32>,
    /// path -> the identity of the object currently at that path.
    ///
    /// MINTED at creation and thereafter only MOVED, never derived from the path. An
    /// earlier draft derived it by hashing the path, which cannot be made correct:
    /// identity must be invariant for an object AND distinct across objects, and a
    /// path hash gives you one or the other. Carrying it on rename freed the old
    /// hash, so a file recreated at the source path collided with the renamed one -
    /// MEASURED, two distinct objects reported the same id.
    identities: HashMap<PathBuf, flux_fs::FileIdentity>,
    /// Pre-incremented, so the first object is 1 and nothing ever gets 0 - §107
    /// forbids treating a zero id as a valid identity.
    next_object: u128,
}

/// `NotFound` when the rename's source does not exist, as `std::fs::rename` gives.
///
/// Without it `move_object`'s `unwrap_or_default()` CONJURED the destination: renaming
/// a missing path returned `Ok`, left an empty file at `to`, and minted no identity for
/// it, so the next `metadata` on that path hit the unreachable-by-construction panic.
/// MEASURED before this guard: `Ok`, `/dest` existed, `metadata` panicked.
fn missing_source(g: &Inner, from: &Path) -> Result<()> {
    if g.files.contains_key(from) {
        return Ok(());
    }
    Err(FsError::new(Code::IoError, std::io::Error::from(std::io::ErrorKind::NotFound)))
}

/// Give the object now at `path` a fresh identity, unless that path already holds
/// one - writing over an existing path is a truncate, not a new object.
///
/// EVERY creation path must call this: `write_file`, `create_new` and `add_special`,
/// which all insert into `files`. `metadata` panics rather than inventing an identity
/// for a path that skipped it, because the plausible alternative - reporting
/// `Unavailable` - is a state the walker handles GRACEFULLY, so an unhooked path
/// would silently degrade while its test stayed green.
fn mint_identity(g: &mut Inner, path: &Path) {
    if g.identities.contains_key(path) {
        return;
    }

    g.next_object += 1;
    let id = flux_fs::ObjectId { volume: 1, index: g.next_object };
    g.identities.insert(path.to_path_buf(), flux_fs::FileIdentity::Strong(id));
}

/// Move a name's content AND its metadata. Moving only the bytes meant the times and
/// permissions applied to the temporary vanished at publication, so no test could
/// assert that the §44.1 ordering achieved anything.
fn move_object(g: &mut Inner, from: &Path, to: &Path) {
    let bytes = g.files.remove(from).unwrap_or_default();
    g.files.insert(to.to_path_buf(), bytes);
    // REPLACE, never merge. A rename destroys the object at `to`, so where the source
    // carries nothing the destination's old value is REMOVED rather than left standing.
    // MEASURED before this: renaming onto a path left the destination's own permissions
    // in place, so the two objects merged instead of one replacing the other.
    match g.times.remove(from) {
        Some(v) => {
            g.times.insert(to.to_path_buf(), v);
        }
        None => {
            g.times.remove(to);
        }
    }
    match g.perms.remove(from) {
        Some(v) => {
            g.perms.insert(to.to_path_buf(), v);
        }
        None => {
            g.perms.remove(to);
        }
    }
    // Identity follows the OBJECT, not the name: a real filesystem preserves the inode
    // across a rename, and §149.4's whole point is detecting when the object under a
    // path CHANGED.
    match g.identities.remove(from) {
        Some(v) => {
            g.identities.insert(to.to_path_buf(), v);
        }
        None => {
            g.identities.remove(to);
        }
    }
}

#[derive(Default)]
pub struct FaultFs {
    inner: std::sync::Arc<Mutex<Inner>>,
}

pub struct FakeHandle {
    path: PathBuf,
    buf: Vec<u8>,
    read_pos: usize,
    /// Where writes land. `None` for a handle opened to read, so a reader cannot
    /// mutate the fake's state even though both roles share this struct.
    sink: Option<std::sync::Arc<Mutex<Inner>>>,
    sync_fault: std::sync::Arc<Mutex<Option<Code>>>,
    write_fault: std::sync::Arc<Mutex<Option<std::io::Error>>>,
}

impl Read for FakeHandle {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        let n = (self.buf.len() - self.read_pos).min(out.len());
        out[..n].copy_from_slice(&self.buf[self.read_pos..self.read_pos + n]);
        self.read_pos += n;
        Ok(n)
    }
}

impl Write for FakeHandle {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        if let Some(e) = self.write_fault.lock().unwrap().take() {
            return Err(e);
        }
        self.buf.extend_from_slice(b);
        // Straight through to the fake's state. Without this the bytes died in the
        // handle, `rename_*` published an empty file, and every test still passed
        // because none of them looked at the content.
        if let Some(sink) = &self.sink {
            let mut g = sink.lock().unwrap();
            g.files.entry(self.path.clone()).or_default().extend_from_slice(b);
        }
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl FileHandle for FakeHandle {
    fn sync_all(&self) -> Result<()> {
        // Record it. Without this the call log never mentioned `sync_all`, so no test
        // could tell a run that synced from one that did not -- only one that FAILED
        // to, via the injected fault below.
        if let Some(sink) = &self.sink {
            sink.lock().unwrap().calls.push(format!("sync_all({})", self.path.display()));
        }
        if let Some(code) = self.sync_fault.lock().unwrap().take() {
            return Err(FsError::new(code, std::io::Error::other("injected")));
        }
        Ok(())
    }
}

impl FaultFs {
    pub fn new() -> Self {
        Self::default()
    }

    /// Seed a source file.
    pub fn write_file(&self, path: impl AsRef<Path>, bytes: &[u8]) {
        let path = path.as_ref();
        let mut g = self.inner.lock().unwrap();
        g.files.insert(path.to_path_buf(), bytes.to_vec());
        mint_identity(&mut g, path);
    }

    pub fn exists(&self, path: impl AsRef<Path>) -> bool {
        let path = path.as_ref();
        let g = self.inner.lock().unwrap();
        g.files.contains_key(path) || g.directories.contains(path)
    }

    /// Override what `metadata` reports a path as, WITHOUT changing what `read_dir`
    /// lists it as. That split is the whole point: it is what an object swapped
    /// between the listing and the stat looks like from inside the walk.
    pub fn set_type(&self, path: impl AsRef<Path>, file_type: FileType) {
        let path = path.as_ref();
        self.inner.lock().unwrap().types.insert(path.to_path_buf(), file_type);
    }

    /// Inject a fault whose source carries a CHOSEN `ErrorKind`.
    ///
    /// `fail` alone always produced `ErrorKind::Other`, so any code branching on the
    /// kind -- `discard`'s `NotFound` arm, `copy_file`'s step-7 `NotFound` arm -- could
    /// not be reached through injection at all. Measured with `cargo mutants`: both
    /// guards survived mutation because no test could express the input that
    /// distinguishes them.
    pub fn fail_kind(&self, name: &str, code: Code, kind: std::io::ErrorKind) {
        let mut g = self.inner.lock().unwrap();
        g.faults.insert(name.to_string(), code);
        g.fault_kinds.insert(name.to_string(), kind);
    }

    /// Inject a fault on the Nth call of `name` rather than the first, so a step that
    /// calls the same method twice can have its LATER call fail.
    pub fn fail_nth(&self, name: &str, nth: u32, code: Code, kind: std::io::ErrorKind) {
        let mut g = self.inner.lock().unwrap();
        g.nth_faults.insert(name.to_string(), (nth, code, kind));
    }

    /// Make the next call to `name` fail with `code`, once.
    pub fn fail(&self, name: &str, code: Code) {
        self.inner.lock().unwrap().faults.insert(name.to_string(), code);
    }

    /// Fail EVERY call to `name`. Needed for `remove_file`: the leftover sweep calls
    /// it before anything else, so a one-shot fault is consumed there and can never
    /// reach the cleanup that `discard` performs.
    pub fn fail_always(&self, name: &str, code: Code) {
        self.inner.lock().unwrap().always.insert(name.to_string(), code);
    }

    pub fn calls(&self) -> Vec<String> {
        self.inner.lock().unwrap().calls.clone()
    }

    pub fn called(&self, prefix: &str) -> bool {
        self.calls().iter().any(|c| c.starts_with(prefix))
    }

    /// The bytes the fake currently holds for `path`.
    pub fn read_file(&self, path: impl AsRef<Path>) -> Option<Vec<u8>> {
        let path = path.as_ref();
        self.inner.lock().unwrap().files.get(path).cloned()
    }

    /// Delete a file the *second* time its metadata is read -- a source removed
    /// while the copy was streaming.
    pub fn vanish_on_second_metadata(&self, path: impl AsRef<Path>) {
        let path = path.as_ref();
        let mut g = self.inner.lock().unwrap();
        g.vanish.insert(path.to_path_buf());
    }

    /// The permissions last applied to `path`.
    pub fn permissions(&self, path: impl AsRef<Path>) -> Option<Perms> {
        let path = path.as_ref();
        self.inner.lock().unwrap().perms.get(path).copied().flatten()
    }

    /// The modified time last applied to `path`.
    pub fn modified(&self, path: impl AsRef<Path>) -> Option<SystemTime> {
        let path = path.as_ref();
        self.inner.lock().unwrap().times.get(path).copied().flatten()
    }

    /// Give a path an identity. Two paths given the SAME `ObjectId` is how a test
    /// reproduces a directory cycle with no mount and no privilege.
    pub fn set_identity(&self, path: impl AsRef<Path>, identity: flux_fs::FileIdentity) {
        let path = path.as_ref();
        self.inner.lock().unwrap().identities.insert(path.to_path_buf(), identity);
    }

    /// Make `metadata` report `path` as something other than a regular file -- a
    /// directory, a symlink, a device. Without this the fake reported `is_file: true`
    /// for everything and SPECIAL_FILE_UNSUPPORTED had no test that produced it.
    pub fn add_special(&self, path: impl AsRef<Path>) {
        let path = path.as_ref();
        let mut g = self.inner.lock().unwrap();
        g.files.insert(path.to_path_buf(), Vec::new());
        g.types.insert(path.to_path_buf(), FileType::Other);
        mint_identity(&mut g, path);
    }

    /// Seed a symlink. The fake never follows it -- the walk must REPORT symlinks
    /// and never descend into them, so a target would be unused state.
    pub fn add_symlink(&self, path: impl AsRef<Path>) {
        let path = path.as_ref();
        let mut g = self.inner.lock().unwrap();
        g.files.insert(path.to_path_buf(), Vec::new());
        g.types.insert(path.to_path_buf(), FileType::Symlink);
        mint_identity(&mut g, path);
    }

    /// Give a file permissions, so a copy has something to carry across.
    pub fn set_file_perms(&self, path: impl AsRef<Path>, perms: Perms) {
        let path = path.as_ref();
        self.inner.lock().unwrap().perms.insert(path.to_path_buf(), Some(perms));
    }

    /// Fail the next `write` on a handle from `create_new`, with this error.
    pub fn fail_write(&self, e: std::io::Error) {
        self.inner.lock().unwrap().write_fault = Some(e);
    }

    /// Append to a file the *second* time its metadata is read — what a concurrent
    /// writer looks like from inside step 6.
    pub fn grow_on_second_metadata(&self, path: impl AsRef<Path>, extra: &[u8]) {
        let path = path.as_ref();
        let mut g = self.inner.lock().unwrap();
        g.grow.insert(path.to_path_buf(), extra.to_vec());
    }

    fn record(&self, call: String, key: &str) -> Result<()> {
        let mut g = self.inner.lock().unwrap();
        g.calls.push(call);
        let n = {
            let c = g.call_counts.entry(key.to_string()).or_insert(0);
            *c += 1;
            *c
        };
        if let Some(&(nth, code, kind)) = g.nth_faults.get(key)
            && n == nth
        {
            return Err(FsError::new(code, std::io::Error::new(kind, "injected")));
        }
        if let Some(code) = g.always.get(key).copied() {
            return Err(Self::injected(code, g.fault_kinds.get(key).copied()));
        }
        if let Some(code) = g.faults.remove(key) {
            return Err(Self::injected(code, g.fault_kinds.remove(key)));
        }
        Ok(())
    }

    /// `ErrorKind::Other` unless a test asked for a specific kind.
    fn injected(code: Code, kind: Option<std::io::ErrorKind>) -> FsError {
        match kind {
            Some(k) => FsError::new(code, std::io::Error::new(k, "injected")),
            None => FsError::new(code, std::io::Error::other("injected")),
        }
    }
}

impl FileSystem for FaultFs {
    // One concrete type serves both roles here, and that is fine: `copy_file` is
    // generic over `FileSystem`, so inside it `F::Reader` is known only to be `Read`
    // no matter what the concrete type also implements. The fake does not need two
    // structs to give the algorithm the compile-time separation.
    type Reader = FakeHandle;
    type Writer = FakeHandle;

    fn open_read(&self, path: &Path) -> Result<Self::Reader> {
        let p = path.to_path_buf();
        self.record(format!("open_read({})", p.display()), "open_read")?;
        let g = self.inner.lock().unwrap();
        let buf = g.files.get(&p).cloned().ok_or_else(|| {
            FsError::new(Code::IoError, std::io::Error::from(std::io::ErrorKind::NotFound))
        })?;
        // The reader never syncs, and never writes: only the writer may consume the
        // injected sync fault, and only the writer gets a sink.
        Ok(FakeHandle {
            path: p,
            buf,
            read_pos: 0,
            sink: None,
            sync_fault: std::sync::Arc::new(Mutex::new(None)),
            write_fault: std::sync::Arc::new(Mutex::new(None)),
        })
    }

    fn create_new(&self, path: &Path) -> Result<Self::Writer> {
        let p = path.to_path_buf();
        self.record(format!("create_new({})", p.display()), "create_new")?;
        let mut g = self.inner.lock().unwrap();
        if g.files.contains_key(&p) {
            return Err(FsError::new(
                Code::IoError,
                std::io::Error::from(std::io::ErrorKind::AlreadyExists),
            ));
        }
        g.files.insert(p.clone(), Vec::new());
        mint_identity(&mut g, &p);
        // Take the injected fault from the guard already held. A helper that locks
        // `inner` again would DEADLOCK here -- `std::sync::Mutex` is not reentrant --
        // and every test that creates a temporary would hang forever.
        let sync_fault = std::sync::Arc::new(Mutex::new(g.faults.remove("sync_all")));
        let write_fault = std::sync::Arc::new(Mutex::new(g.write_fault.take()));
        drop(g);
        Ok(FakeHandle {
            path: p,
            buf: Vec::new(),
            read_pos: 0,
            sink: Some(std::sync::Arc::clone(&self.inner)),
            sync_fault,
            write_fault,
        })
    }

    fn metadata(&self, path: &Path) -> Result<Metadata> {
        let p = path.to_path_buf();
        self.record(format!("metadata({})", p.display()), "metadata")?;
        let mut g = self.inner.lock().unwrap();
        // Copy the count out: holding the entry's `&mut` across the blocks below
        // borrows `g` for too long, and they each need it again.
        let reads = {
            let c = g.metadata_reads.entry(p.clone()).or_insert(0);
            *c += 1;
            *c
        };
        if reads == 2
            && let Some(extra) = g.grow.get(&p).cloned()
            && let Some(b) = g.files.get_mut(&p)
        {
            b.extend_from_slice(&extra);
        }
        if reads == 2 && g.vanish.contains(&p) {
            g.files.remove(&p);
        }
        let g = &*g;
        let len = match g.files.get(&p) {
            Some(b) => b.len() as u64,
            // A directory has no bytes, and is not in `files`.
            None if g.directories.contains(&p) => 0,
            None => {
                return Err(FsError::new(
                    Code::IoError,
                    std::io::Error::from(std::io::ErrorKind::NotFound),
                ));
            }
        };
        Ok(Metadata {
            len,
            file_type: g.types.get(&p).copied().unwrap_or(FileType::File),
            permissions: g.perms.get(&p).copied().flatten(),
            modified: g.times.get(&p).copied().flatten(),
            // Panic, not `Unavailable`: `Unavailable` is a state the walker handles
            // gracefully, so an unhooked creation path would degrade silently and keep
            // its test green. Every path that inserts into `files` calls
            // `mint_identity`, which makes this unreachable - loudly, if it ever is not.
            identity: *g.identities.get(&p).unwrap_or_else(|| {
                panic!(
                    "no identity minted for {}: a creation path skipped mint_identity",
                    p.display()
                )
            }),
        })
    }

    fn set_times(&self, file: &Self::Writer, modified: Option<SystemTime>) -> Result<()> {
        self.record(format!("set_times({})", file.path.display()), "set_times")?;
        self.inner.lock().unwrap().times.insert(file.path.clone(), modified);
        Ok(())
    }

    fn set_permissions(&self, file: &Self::Writer, perms: Option<Perms>) -> Result<()> {
        self.record(format!("set_permissions({})", file.path.display()), "set_permissions")?;
        // Record it. Dropping the argument made the whole suite blind to permissions:
        // a `copy_file` that passed `None` every time passed every test.
        self.inner.lock().unwrap().perms.insert(file.path.clone(), perms);
        Ok(())
    }

    fn rename_replace(&self, from: &Path, to: &Path) -> Result<()> {
        let (f, t) = (from.to_path_buf(), to.to_path_buf());
        self.record(
            format!("rename_replace({} -> {})", f.display(), t.display()),
            "rename_replace",
        )?;
        let mut g = self.inner.lock().unwrap();
        missing_source(&g, &f)?;
        move_object(&mut g, &f, &t);
        Ok(())
    }

    fn rename_no_replace(&self, from: &Path, to: &Path) -> Result<()> {
        let (f, t) = (from.to_path_buf(), to.to_path_buf());
        self.record(
            format!("rename_no_replace({} -> {})", f.display(), t.display()),
            "rename_no_replace",
        )?;
        let mut g = self.inner.lock().unwrap();
        missing_source(&g, &f)?;
        if g.files.contains_key(&t) {
            return Err(FsError::new(
                Code::IoError,
                std::io::Error::from(std::io::ErrorKind::AlreadyExists),
            ));
        }
        move_object(&mut g, &f, &t);
        Ok(())
    }

    fn remove_file(&self, path: &Path) -> Result<()> {
        let p = path.to_path_buf();
        self.record(format!("remove_file({})", p.display()), "remove_file")?;
        // NotFound when it is not there, because `std::fs::remove_file` does that and
        // `discard` branches on it. A fake that returned Ok here would make that
        // branch untestable and hide the difference.
        let mut g = self.inner.lock().unwrap();
        if g.files.remove(&p).is_none() {
            return Err(FsError::new(
                Code::IoError,
                std::io::Error::from(std::io::ErrorKind::NotFound),
            ));
        }
        // The object is gone, so its state goes with it. Leaving these behind let a
        // later file created at the SAME path inherit a dead object's identity and
        // permissions - MEASURED, a recreated path reported the removed file's perms.
        g.identities.remove(&p);
        g.times.remove(&p);
        g.perms.remove(&p);
        Ok(())
    }

    fn read_dir(&self, path: &Path) -> Result<Vec<DirEntry>> {
        let p = path.to_path_buf();
        self.record(format!("read_dir({})", p.display()), "read_dir")?;
        let g = self.inner.lock().unwrap();
        if !g.directories.contains(&p) {
            return Err(FsError::new(
                Code::IoError,
                std::io::Error::from(std::io::ErrorKind::NotFound),
            ));
        }
        // Immediate children only, from both maps. Unsorted deliberately: the trait
        // says ordering is the caller's job, and a fake that pre-sorted would let a
        // walk with NO sort pass its ordering test.
        let mut out = Vec::new();
        for key in g.files.keys().chain(g.directories.iter()) {
            if key.parent() != Some(p.as_path()) {
                continue;
            }
            let Some(name) = key.file_name() else { continue };
            let file_type = if g.directories.contains(key) {
                FileType::Dir
            } else {
                g.types.get(key).copied().unwrap_or(FileType::File)
            };
            out.push(DirEntry { name: name.to_os_string(), file_type });
        }
        Ok(out)
    }

    fn create_dir(&self, path: &Path) -> Result<()> {
        let p = path.to_path_buf();
        self.record(format!("create_dir({})", p.display()), "create_dir")?;
        let mut g = self.inner.lock().unwrap();
        if g.directories.contains(&p) || g.files.contains_key(&p) {
            return Err(FsError::new(
                Code::IoError,
                std::io::Error::from(std::io::ErrorKind::AlreadyExists),
            ));
        }
        // Fails if the parent is missing, as the trait says and `std::fs::create_dir`
        // does. The walk root itself has no parent in the fake, so a root-level
        // create is allowed.
        if let Some(parent) = p.parent()
            && !parent.as_os_str().is_empty()
            && parent != Path::new("/")
            && !g.directories.contains(parent)
        {
            return Err(FsError::new(
                Code::IoError,
                std::io::Error::from(std::io::ErrorKind::NotFound),
            ));
        }
        g.directories.insert(p.clone());
        g.types.insert(p.clone(), FileType::Dir);
        mint_identity(&mut g, &p);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_records_the_order_of_calls() {
        let fs = FaultFs::new();
        fs.write_file("/a", b"hi");
        let _ = fs.metadata(std::path::Path::new("/a"));
        let _ = fs.remove_file(std::path::Path::new("/a"));
        assert_eq!(fs.calls(), vec!["metadata(/a)".to_string(), "remove_file(/a)".to_string()]);
    }

    #[test]
    fn it_fails_the_named_call() {
        let fs = FaultFs::new();
        fs.fail("remove_file", flux_fs::Code::PermissionDenied);
        let e = fs.remove_file(std::path::Path::new("/a")).unwrap_err();
        assert_eq!(e.code, flux_fs::Code::PermissionDenied);
    }

    #[test]
    fn identities_default_to_distinct_and_can_be_forced_equal() {
        use flux_fs::{FileIdentity, ObjectId};
        let fs = FaultFs::new();
        fs.write_file("/a", b"x");
        fs.write_file("/b", b"x");

        // Default: every path is its own object, so an ordinary test needs no setup
        // and two unrelated paths never collide by accident.
        let ia = fs.metadata(std::path::Path::new("/a")).unwrap().identity;
        let ib = fs.metadata(std::path::Path::new("/b")).unwrap().identity;
        assert!(matches!(ia, FileIdentity::Strong(_)));
        assert_ne!(ia, ib);

        // Forced: this is what makes a cycle testable without a mount or a privilege.
        let shared = ObjectId { volume: 7, index: 7 };
        fs.set_identity("/a", FileIdentity::Strong(shared));
        fs.set_identity("/b", FileIdentity::Strong(shared));
        assert_eq!(
            fs.metadata(std::path::Path::new("/a")).unwrap().identity,
            fs.metadata(std::path::Path::new("/b")).unwrap().identity
        );

        // And the degraded path is reachable, which the walker's fallback needs.
        fs.set_identity("/a", FileIdentity::Unavailable);
        assert_eq!(
            fs.metadata(std::path::Path::new("/a")).unwrap().identity,
            FileIdentity::Unavailable
        );
    }

    #[test]
    fn writing_over_an_existing_path_preserves_its_identity() {
        // A truncating write is the SAME object with new content, so its identity must
        // not move - that is what `mint_identity`'s early return is for, and nothing
        // exercised it. MEASURED: deleting that early return left the whole suite green
        // at 36 passed, so a write could have silently re-minted and no test would have
        // noticed. Object continuity across a write is what a walk comparing a
        // directory against its ancestors depends on.
        let fs = FaultFs::new();
        fs.write_file("/a", b"first");
        let before = fs.metadata(std::path::Path::new("/a")).unwrap().identity;

        fs.write_file("/a", b"second and longer");
        let after = fs.metadata(std::path::Path::new("/a")).unwrap().identity;

        assert_eq!(before, after, "a write replaces content, not the object");
        assert_eq!(fs.read_file("/a").as_deref(), Some(&b"second and longer"[..]));
    }

    #[test]
    fn renaming_a_missing_source_fails_instead_of_conjuring_the_destination() {
        // `move_object`'s `unwrap_or_default()` created the destination out of nothing,
        // so a rename of a missing path reported success, left an empty file behind, and
        // minted no identity for it - which then turned the next `metadata` into a panic.
        // MEASURED before the guard: Ok, /dest existed, metadata panicked.
        let fs = FaultFs::new();
        let e = fs
            .rename_replace(std::path::Path::new("/missing"), std::path::Path::new("/dest"))
            .unwrap_err();

        assert_eq!(e.source.kind(), std::io::ErrorKind::NotFound, "as std::fs::rename gives");
        assert!(!fs.exists("/dest"), "a failed rename creates nothing");
    }

    #[test]
    fn a_path_recreated_after_a_rename_is_a_different_object() {
        // The defect the path-hash model could not avoid: carrying the identity to the
        // new name FREED the old path's hash, so a file created at the old path minted
        // the same id as the renamed one. MEASURED at the time: collide=true, two
        // distinct objects reporting one identity.
        let fs = FaultFs::new();
        fs.write_file("/a", b"x");
        fs.rename_replace(std::path::Path::new("/a"), std::path::Path::new("/b")).unwrap();
        fs.write_file("/a", b"y");

        let a = fs.metadata(std::path::Path::new("/a")).unwrap().identity;
        let b = fs.metadata(std::path::Path::new("/b")).unwrap().identity;
        assert_ne!(a, b, "a new file at a freed name is not the object that moved away");
    }

    #[test]
    fn a_rename_replaces_the_destination_rather_than_merging_with_it() {
        // A rename destroys the object at the destination. Metadata the source does not
        // carry must not survive on it - MEASURED before the fix, the destination's own
        // permissions were still there afterwards, so the two objects had merged.
        let fs = FaultFs::new();
        fs.write_file("/a", b"a");
        fs.write_file("/b", b"b");
        fs.set_file_perms("/b", Perms::UnixMode(0o600));

        fs.rename_replace(std::path::Path::new("/a"), std::path::Path::new("/b")).unwrap();

        assert_eq!(fs.permissions("/b"), None, "the destination's own permissions died with it");
    }

    #[test]
    fn removing_a_path_does_not_leak_its_state_to_a_later_file() {
        // `remove_file` used to drop only the CONTENT, so a later file at the same path
        // inherited a dead object's permissions and identity.
        let fs = FaultFs::new();
        fs.write_file("/a", b"x");
        fs.set_file_perms("/a", Perms::UnixMode(0o600));
        let first = fs.metadata(std::path::Path::new("/a")).unwrap().identity;
        fs.remove_file(std::path::Path::new("/a")).unwrap();

        fs.write_file("/a", b"new");
        assert_eq!(fs.permissions("/a"), None, "a dead object's permissions are not inherited");
        assert_ne!(
            fs.metadata(std::path::Path::new("/a")).unwrap().identity,
            first,
            "the same name twice is two objects, not one"
        );
    }

    #[test]
    fn an_identity_follows_the_object_through_a_rename() {
        // A real filesystem preserves the inode across a rename; the name moves, the
        // object does not change. The fake derived identity from the PATH, so a rename
        // silently minted a new object - which would make §149.4's "did the object under
        // this path change?" unanswerable through this fake.
        let fs = FaultFs::new();
        fs.write_file("/a", b"x");
        let before = fs.metadata(std::path::Path::new("/a")).unwrap().identity;

        fs.rename_replace(std::path::Path::new("/a"), std::path::Path::new("/b")).unwrap();
        let after = fs.metadata(std::path::Path::new("/b")).unwrap().identity;

        assert_eq!(before, after, "a rename moves the name, not the object");
    }

    #[test]
    fn create_dir_then_read_dir_round_trips_through_the_trait() {
        // C1: the walk's fixtures are built with create_dir, so it has a consumer in
        // this PR rather than waiting for copy_tree.
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/a")).unwrap();
        fs.create_dir(Path::new("/a/sub")).unwrap();
        fs.write_file("/a/f", b"x");
        fs.add_symlink("/a/link");

        let mut got: Vec<(String, FileType)> = fs
            .read_dir(Path::new("/a"))
            .unwrap()
            .into_iter()
            .map(|e| (e.name.to_string_lossy().into_owned(), e.file_type))
            .collect();
        got.sort_by(|a, b| a.0.cmp(&b.0));

        assert_eq!(
            got,
            vec![
                ("f".to_string(), FileType::File),
                ("link".to_string(), FileType::Symlink),
                ("sub".to_string(), FileType::Dir),
            ]
        );
    }

    #[test]
    fn an_empty_directory_is_distinguishable_from_a_special_file() {
        // The design document names this as the reason the fake had to change: the
        // old `not_files` set made an empty directory and a device node identical.
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/d")).unwrap();
        fs.add_special("/dev");

        assert!(fs.read_dir(Path::new("/d")).unwrap().is_empty());
        assert_eq!(fs.metadata(Path::new("/d")).unwrap().file_type, FileType::Dir);
        assert_eq!(fs.metadata(Path::new("/dev")).unwrap().file_type, FileType::Other);
        assert!(fs.read_dir(Path::new("/dev")).is_err());
    }

    #[test]
    fn create_dir_refuses_a_missing_parent_and_an_occupied_name() {
        let fs = FaultFs::new();
        assert!(fs.create_dir(Path::new("/a/b")).is_err(), "parent /a does not exist");
        fs.create_dir(Path::new("/a")).unwrap();
        assert!(fs.create_dir(Path::new("/a")).is_err(), "already a directory");
        fs.write_file("/f", b"x");
        assert!(fs.create_dir(Path::new("/f")).is_err(), "occupied by a file");
    }
}
