//! A filesystem that does what you tell it and remembers what you asked.
//!
//! The spec's rules are about ORDER — metadata before publication, no publication
//! after a strict failure — so this records the call sequence and can fail any
//! named call. None of that is reachable against a real disk.

use flux_fs::{Code, FileHandle, FileSystem, FsError, Metadata, Perms, Result};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::Mutex;
use std::time::SystemTime;

#[derive(Default)]
struct Inner {
    files: HashMap<String, Vec<u8>>,
    calls: Vec<String>,
    faults: HashMap<String, Code>,
    times: HashMap<String, Option<SystemTime>>,
    /// path -> bytes appended on the second `metadata` call
    grow: HashMap<String, Vec<u8>>,
    metadata_reads: HashMap<String, u32>,
    /// call name -> code, not consumed on use
    always: HashMap<String, Code>,
    /// paths removed on the second `metadata` call
    vanish: std::collections::HashSet<String>,
    /// paths `metadata` reports as not a regular file
    not_files: std::collections::HashSet<String>,
    perms: HashMap<String, Option<Perms>>,
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
}

/// Move a name's content AND its metadata. Moving only the bytes meant the times and
/// permissions applied to the temporary vanished at publication, so no test could
/// assert that the §44.1 ordering achieved anything.
fn move_object(g: &mut Inner, from: &str, to: &str) {
    let bytes = g.files.remove(from).unwrap_or_default();
    g.files.insert(to.to_string(), bytes);
    if let Some(v) = g.times.remove(from) {
        g.times.insert(to.to_string(), v);
    }
    if let Some(v) = g.perms.remove(from) {
        g.perms.insert(to.to_string(), v);
    }
}

#[derive(Default)]
pub struct FaultFs {
    inner: std::sync::Arc<Mutex<Inner>>,
}

pub struct FakeHandle {
    path: String,
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
            sink.lock().unwrap().calls.push(format!("sync_all({})", self.path));
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
    pub fn write_file(&self, path: &str, bytes: &[u8]) {
        self.inner.lock().unwrap().files.insert(path.to_string(), bytes.to_vec());
    }

    pub fn exists(&self, path: &str) -> bool {
        self.inner.lock().unwrap().files.contains_key(path)
    }

    /// Make the next call to `name` fail with `code`.
    /// Fail the next call to `name`, once.
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
    pub fn read_file(&self, path: &str) -> Option<Vec<u8>> {
        self.inner.lock().unwrap().files.get(path).cloned()
    }

    /// Delete a file the *second* time its metadata is read -- a source removed
    /// while the copy was streaming.
    pub fn vanish_on_second_metadata(&self, path: &str) {
        let mut g = self.inner.lock().unwrap();
        g.vanish.insert(path.to_string());
    }

    /// The permissions last applied to `path`.
    pub fn permissions(&self, path: &str) -> Option<Perms> {
        self.inner.lock().unwrap().perms.get(path).copied().flatten()
    }

    /// The modified time last applied to `path`.
    pub fn modified(&self, path: &str) -> Option<SystemTime> {
        self.inner.lock().unwrap().times.get(path).copied().flatten()
    }

    /// Make `metadata` report `path` as something other than a regular file -- a
    /// directory, a symlink, a device. Without this the fake reported `is_file: true`
    /// for everything and SPECIAL_FILE_UNSUPPORTED had no test that produced it.
    pub fn add_special(&self, path: &str) {
        let mut g = self.inner.lock().unwrap();
        g.files.insert(path.to_string(), Vec::new());
        g.not_files.insert(path.to_string());
    }

    /// Give a file permissions, so a copy has something to carry across.
    pub fn set_file_perms(&self, path: &str, perms: Perms) {
        self.inner.lock().unwrap().perms.insert(path.to_string(), Some(perms));
    }

    /// Fail the next `write` on a handle from `create_new`, with this error.
    pub fn fail_write(&self, e: std::io::Error) {
        self.inner.lock().unwrap().write_fault = Some(e);
    }

    /// Append to a file the *second* time its metadata is read — what a concurrent
    /// writer looks like from inside step 6.
    pub fn grow_on_second_metadata(&self, path: &str, extra: &[u8]) {
        let mut g = self.inner.lock().unwrap();
        g.grow.insert(path.to_string(), extra.to_vec());
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
        let p = path.display().to_string();
        self.record(format!("open_read({p})"), "open_read")?;
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
        let p = path.display().to_string();
        self.record(format!("create_new({p})"), "create_new")?;
        let mut g = self.inner.lock().unwrap();
        if g.files.contains_key(&p) {
            return Err(FsError::new(
                Code::IoError,
                std::io::Error::from(std::io::ErrorKind::AlreadyExists),
            ));
        }
        g.files.insert(p.clone(), Vec::new());
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
        let p = path.display().to_string();
        self.record(format!("metadata({p})"), "metadata")?;
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
        let len = g.files.get(&p).map(|b| b.len() as u64).ok_or_else(|| {
            FsError::new(Code::IoError, std::io::Error::from(std::io::ErrorKind::NotFound))
        })?;
        Ok(Metadata {
            len,
            is_file: !g.not_files.contains(&p),
            permissions: g.perms.get(&p).copied().flatten(),
            modified: g.times.get(&p).copied().flatten(),
        })
    }

    fn set_times(&self, file: &Self::Writer, modified: Option<SystemTime>) -> Result<()> {
        self.record(format!("set_times({})", file.path), "set_times")?;
        self.inner.lock().unwrap().times.insert(file.path.clone(), modified);
        Ok(())
    }

    fn set_permissions(&self, file: &Self::Writer, perms: Option<Perms>) -> Result<()> {
        self.record(format!("set_permissions({})", file.path), "set_permissions")?;
        // Record it. Dropping the argument made the whole suite blind to permissions:
        // a `copy_file` that passed `None` every time passed every test.
        self.inner.lock().unwrap().perms.insert(file.path.clone(), perms);
        Ok(())
    }

    fn rename_replace(&self, from: &Path, to: &Path) -> Result<()> {
        let (f, t) = (from.display().to_string(), to.display().to_string());
        self.record(format!("rename_replace({f} -> {t})"), "rename_replace")?;
        move_object(&mut self.inner.lock().unwrap(), &f, &t);
        Ok(())
    }

    fn rename_no_replace(&self, from: &Path, to: &Path) -> Result<()> {
        let (f, t) = (from.display().to_string(), to.display().to_string());
        self.record(format!("rename_no_replace({f} -> {t})"), "rename_no_replace")?;
        let mut g = self.inner.lock().unwrap();
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
        let p = path.display().to_string();
        self.record(format!("remove_file({p})"), "remove_file")?;
        // NotFound when it is not there, because `std::fs::remove_file` does that and
        // `discard` branches on it. A fake that returned Ok here would make that
        // branch untestable and hide the difference.
        if self.inner.lock().unwrap().files.remove(&p).is_none() {
            return Err(FsError::new(
                Code::IoError,
                std::io::Error::from(std::io::ErrorKind::NotFound),
            ));
        }
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
}
