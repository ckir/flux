//! A filesystem that does what you tell it and remembers what you asked.
//!
//! The spec's rules are about ORDER — metadata before publication, no publication
//! after a strict failure — so this records the call sequence and can fail any
//! named call. None of that is reachable against a real disk.

use flux_fs::{
    ClaimKey, ClaimOutcome, ClaimRecord, ClaimStore, Code, DestinationRoot, DirEntry, DirHandle,
    Durability, FileHandle, FileSystem, FileType, FluxPathKey, FsError, LockCapability, LockFile,
    Metadata, MountRoot, Perms, Result, check_component,
};
use std::collections::{HashMap, HashSet};
use std::ffi::{OsStr, OsString};
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
    /// directory snapshot path -> what `mount_root` answers (absent: `No`)
    mount_roots: HashMap<PathBuf, MountRoot>,
    /// directory snapshot path -> what `canonical_path` answers (absent: the path itself)
    canonical: HashMap<PathBuf, PathBuf>,
    /// directory snapshot path -> the fault `canonical_path` raises on that one directory, every time
    canonical_faults: HashMap<PathBuf, (Code, std::io::ErrorKind)>,
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
    /// Does this fake's `rename_no_replace` have an atomic no-replace primitive?
    ///
    /// `true` by default, because every test written before this switch existed
    /// assumes it. Setting it `false` is how the engine cut exercises a destination
    /// whose filesystem cannot make the promise, without owning such a filesystem.
    no_replace_support: Option<bool>,
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
    /// The `DirHandle` node graph: id -> node, kept SEPARATE from the path-keyed
    /// maps above. A `DirHandle` reaches a child through the id it already holds --
    /// never by re-walking a path -- which is what lets `repoint_for_test` stage
    /// item 114's attack: rebinding a NAME in some node's `children` does not move
    /// the id a handle minted from that name earlier.
    dir_nodes: HashMap<u64, DirNode>,
    /// path -> node id, consulted ONLY when a path is resolved by name from
    /// scratch: `destination_root`'s argument, and `repoint_for_test`'s two path
    /// arguments. No `DirHandle` method consults this map.
    dir_node_by_path: HashMap<PathBuf, u64>,
    next_dir_node: u64,
    /// Object index -> the `FakeLock` handle id holding its OS-native lock. Keyed by OBJECT, not path: a real
    /// OS-native lock follows the file across a rename, and §240.3 renames a lock aside while holding it.
    lock_holders: HashMap<u128, u64>,
    next_lock_handle: u64,
    /// What `lock_capability` answers; `None` means `LocalStrong`.
    lock_capability: Option<LockCapability>,
    /// call name -> (which call, an action). The action runs just BEFORE that call, outside the fake's own lock,
    /// so it can drive the fake itself: "another process acts between two of this one's calls" (cut 7a Part 2's
    /// protocol tests). Consumed on use.
    #[allow(clippy::type_complexity)]
    hooks: HashMap<String, (u32, Box<dyn FnOnce(&FaultFs) + Send>)>,
    /// Windows' legacy delete (cut 7a Part 1 capstone; `set_legacy_delete`).
    legacy_delete: bool,
    /// Object index -> live `FakeLock` handles on it.
    open_locks: HashMap<u128, u32>,
    /// Paths removed while a lock handle was open under legacy delete: still occupying their name, found by nothing.
    pending: HashSet<PathBuf>,
    /// Every claim store `create_claim_store` made, in creation order. Each holds the encoded claims (`ClaimKey::encode`
    /// -> `ClaimRecord::encode`), the encodings of the real store.
    claim_stores: Vec<ClaimMap>,
}

type ClaimMap = std::sync::Arc<Mutex<std::collections::BTreeMap<Vec<u8>, Vec<u8>>>>;

/// The fake's claim store: the real store's key and value encodings over an in-memory map, with the call log and the
/// fault keys `claim_insert`, `claim_upgrade`, `claim_flush`.
pub struct FakeClaimStore {
    map: ClaimMap,
    inner: std::sync::Arc<Mutex<Inner>>,
}

impl FakeClaimStore {
    fn record(&self, call: String, key: &str) -> Result<()> {
        FaultFs { inner: std::sync::Arc::clone(&self.inner) }.record(call, key)
    }
}

fn corrupt_claim() -> FsError {
    FsError::new(Code::IoError, std::io::Error::other("undecodable claim record"))
}

impl ClaimStore for FakeClaimStore {
    fn insert_if_absent(&mut self, key: &ClaimKey, record: &ClaimRecord) -> Result<ClaimOutcome> {
        self.record(
            format!("claim_insert({})", String::from_utf8_lossy(&key.name)),
            "claim_insert",
        )?;
        let mut m = self.map.lock().unwrap();
        let k = key.encode();
        match m.get(&k) {
            Some(v) => Ok(ClaimOutcome::Present(ClaimRecord::decode(v).ok_or_else(corrupt_claim)?)),
            None => {
                m.insert(k, record.encode());
                Ok(ClaimOutcome::Inserted)
            }
        }
    }

    fn get(&self, key: &ClaimKey) -> Result<Option<ClaimRecord>> {
        let m = self.map.lock().unwrap();
        match m.get(&key.encode()) {
            Some(v) => Ok(Some(ClaimRecord::decode(v).ok_or_else(corrupt_claim)?)),
            None => Ok(None),
        }
    }

    fn upgrade_own_claim(&mut self, key: &ClaimKey, target: &FluxPathKey) -> Result<()> {
        self.record(
            format!("claim_upgrade({})", String::from_utf8_lossy(&key.name)),
            "claim_upgrade",
        )?;
        let mut m = self.map.lock().unwrap();
        let k = key.encode();
        let stored = match m.get(&k) {
            Some(v) => ClaimRecord::decode(v).ok_or_else(corrupt_claim)?,
            None => {
                return Err(FsError::new(
                    Code::IoError,
                    std::io::Error::new(std::io::ErrorKind::NotFound, "no such claim"),
                ));
            }
        };
        if stored.target != *target {
            return Err(FsError::new(
                Code::IoError,
                std::io::Error::other("the claim belongs to another target"),
            ));
        }
        let upgraded = ClaimRecord { target: stored.target, status: flux_fs::ClaimStatus::Created };
        m.insert(k, upgraded.encode());
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        self.record("claim_flush".to_string(), "claim_flush")
    }
}

/// One node in the `DirHandle` graph, addressed by an opaque id rather than by
/// path. `path` is a snapshot taken when the node is minted - moved only when a directory rename moves that directory or an ancestor, since the object moved (cut 7a Part 3a) - and used only to
/// reach the legacy path-keyed maps above (`files`/`directories`/`types`) -- every
/// `DirHandle` method reaches a node through its id and this snapshot, never by
/// re-walking a name. `children` is the one place a name binding lives, and the one
/// place `repoint_for_test` is allowed to write: rewriting an entry there changes
/// what OPENING that name gets from now on, without moving the snapshot any handle
/// that already resolved it is holding.
struct DirNode {
    path: PathBuf,
    children: HashMap<OsString, u64>,
}

/// Get or mint the node for `path`, memoized by path so the same path always yields
/// the same id. This is the only place a path is turned into a node id from
/// scratch -- used by `destination_root` (which resolves its argument by name once,
/// per the trait's own contract) and by `repoint_for_test` (which needs the id of
/// both the name it is rewriting and the id it is rewriting that name onto). No
/// `DirHandle` method calls this: a handle already has its id, and reaches a child
/// through its own node's `children` map, never by feeding a path back through this
/// cache.
fn dir_node_for_path(g: &mut Inner, path: &Path) -> u64 {
    if let Some(&id) = g.dir_node_by_path.get(path) {
        return id;
    }
    g.next_dir_node += 1;
    let id = g.next_dir_node;
    g.dir_nodes.insert(id, DirNode { path: path.to_path_buf(), children: HashMap::new() });
    g.dir_node_by_path.insert(path.to_path_buf(), id);
    id
}

/// `NotFound` when the rename's source does not exist, as `std::fs::rename` gives.
///
/// Without it `move_object`'s `unwrap_or_default()` CONJURED the destination: renaming
/// a missing path returned `Ok`, left an empty file at `to`, and minted no identity for
/// it, so the next `metadata` on that path hit the unreachable-by-construction panic.
/// MEASURED before this guard: `Ok`, `/dest` existed, `metadata` panicked.
fn missing_source(g: &Inner, from: &Path) -> Result<()> {
    if g.files.contains_key(from) || g.directories.contains(from) {
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

/// Forget a removed file: its bytes and everything keyed by its path, so a later object at the path starts fresh.
fn purge(g: &mut Inner, path: &Path) {
    g.files.remove(path);
    g.identities.remove(path);
    g.times.remove(path);
    g.perms.remove(path);
    g.types.remove(path);
}

/// Move a name's content AND its metadata. Moving only the bytes meant the times and
/// permissions applied to the temporary vanished at publication, so no test could
/// assert that the §44.1 ordering achieved anything.
fn move_object(g: &mut Inner, from: &Path, to: &Path) {
    if g.directories.contains(from) {
        move_directory(g, from, to);
        return;
    }
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

/// A directory rename moves its whole subtree, and every handle open on it or below it follows the OBJECT, as on a
/// real filesystem (cut 7a Part 3a: a workspace is built under one name and renamed to another). The name binding moves
/// from the old parent's node to the new parent's.
fn move_directory(g: &mut Inner, from: &Path, to: &Path) {
    let moved = |p: &Path| -> Option<PathBuf> {
        let rest = p.strip_prefix(from).ok()?;
        Some(if rest.as_os_str().is_empty() { to.to_path_buf() } else { to.join(rest) })
    };
    fn rekey<V>(map: &mut HashMap<PathBuf, V>, moved: &dyn Fn(&Path) -> Option<PathBuf>) {
        let keys: Vec<PathBuf> = map.keys().filter(|k| moved(k).is_some()).cloned().collect();
        for key in keys {
            let value = map.remove(&key).expect("listed just above");
            map.insert(moved(&key).expect("filtered just above"), value);
        }
    }
    rekey(&mut g.files, &moved);
    rekey(&mut g.times, &moved);
    rekey(&mut g.perms, &moved);
    rekey(&mut g.identities, &moved);
    rekey(&mut g.types, &moved);
    rekey(&mut g.dir_node_by_path, &moved);
    let dirs: Vec<PathBuf> = g.directories.iter().filter(|d| moved(d).is_some()).cloned().collect();
    for dir in dirs {
        g.directories.remove(&dir);
        g.directories.insert(moved(&dir).expect("filtered just above"));
    }
    for node in g.dir_nodes.values_mut() {
        if let Some(p) = moved(&node.path) {
            node.path = p;
        }
    }
    let node = g.dir_node_by_path.get(to).copied();
    if let (Some(parent), Some(name)) = (from.parent(), from.file_name())
        && let Some(pid) = g.dir_node_by_path.get(parent).copied()
    {
        g.dir_nodes.get_mut(&pid).expect("a bound id names a node").children.remove(name);
    }
    if let (Some(parent), Some(name), Some(id)) = (to.parent(), to.file_name(), node)
        && let Some(pid) = g.dir_node_by_path.get(parent).copied()
    {
        g.dir_nodes
            .get_mut(&pid)
            .expect("a bound id names a node")
            .children
            .insert(name.to_os_string(), id);
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

    fn identity(&self) -> Result<flux_fs::FileIdentity> {
        // The object at the handle's path: a writer's path is its temporary's until the rename, and `move_object`
        // carries the identity across it, so the copy reads this before publishing (cut 7b Part 1 decision 5).
        let Some(sink) = &self.sink else { return Ok(flux_fs::FileIdentity::Unavailable) };
        let mut g = sink.lock().unwrap();
        // `fail("handle_identity", ..)`: one injected failure of this read (cut 7b Part 1 test audit, G4). Its own name:
        // a directory handle's `identity` is a different read.
        if let Some(code) = g.faults.remove("handle_identity") {
            return Err(FsError::new(code, std::io::Error::other("injected")));
        }
        Ok(g.identities.get(&self.path).copied().unwrap_or(flux_fs::FileIdentity::Unavailable))
    }
}

/// A lock file in the fake. It addresses its OBJECT (by identity), so a rename carries it.
pub struct FakeLock {
    object: u128,
    handle: u64,
    inner: std::sync::Arc<Mutex<Inner>>,
}

// Manual, as for `FakeDirHandle`: `Inner` is not `Debug`, and the tests call `unwrap_err()` on `Result<FakeLock, _>`.
impl std::fmt::Debug for FakeLock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FakeLock")
            .field("object", &self.object)
            .field("handle", &self.handle)
            .finish()
    }
}

impl FakeLock {
    /// The path now holding this handle's object, or `NotFound` if none does (the fake does not model reading an
    /// unlinked open file).
    fn path(g: &Inner, object: u128) -> Result<PathBuf> {
        g.identities
            .iter()
            .find(|(_, id)| matches!(id, flux_fs::FileIdentity::Strong(o) if o.index == object))
            .map(|(p, _)| p.clone())
            .ok_or_else(|| {
                FsError::new(Code::IoError, std::io::Error::from(std::io::ErrorKind::NotFound))
            })
    }

    fn record(&self, call: &str) -> Result<()> {
        let fs = FaultFs { inner: std::sync::Arc::clone(&self.inner) };
        let path = { Self::path(&self.inner.lock().unwrap(), self.object).unwrap_or_default() };
        fs.record(format!("{call}({})", path.display()), call)
    }
}

impl LockFile for FakeLock {
    fn try_lock(&self) -> Result<bool> {
        self.record("try_lock")?;
        let mut g = self.inner.lock().unwrap();
        match g.lock_holders.get(&self.object) {
            Some(&h) if h != self.handle => Ok(false),
            _ => {
                g.lock_holders.insert(self.object, self.handle);
                Ok(true)
            }
        }
    }

    fn read_all(&self, limit: usize) -> Result<Vec<u8>> {
        self.record("read_all")?;
        let g = self.inner.lock().unwrap();
        let path = Self::path(&g, self.object)?;
        let mut bytes = g.files.get(&path).cloned().unwrap_or_default();
        bytes.truncate(limit + 1);
        Ok(bytes)
    }

    /// Two steps, as on the real platforms: the write in place, then the cut. Each is a separate recorded call, so a
    /// test can inject a fault at the cut (`lock_set_len`) and see the oversized file a crash there leaves behind.
    fn write_at_start(&self, bytes: &[u8]) -> Result<()> {
        self.record("write_at_start")?;
        {
            let mut g = self.inner.lock().unwrap();
            let path = Self::path(&g, self.object)?;
            let file = g.files.entry(path).or_default();
            if file.len() < bytes.len() {
                file.resize(bytes.len(), 0);
            }
            file[..bytes.len()].copy_from_slice(bytes);
        }
        self.record("lock_set_len")?;
        let mut g = self.inner.lock().unwrap();
        let path = Self::path(&g, self.object)?;
        g.files.get_mut(&path).expect("written just above").truncate(bytes.len());
        Ok(())
    }

    fn sync_all(&self) -> Result<()> {
        self.record("lock_sync_all")
    }

    fn identity(&self) -> Result<flux_fs::FileIdentity> {
        Ok(flux_fs::FileIdentity::Strong(flux_fs::ObjectId { volume: 1, index: self.object }))
    }
}

impl Drop for FakeLock {
    /// Closing the handle releases the OS-native lock, as on every real platform.
    fn drop(&mut self) {
        let Ok(mut g) = self.inner.lock() else { return };
        if g.lock_holders.get(&self.object) == Some(&self.handle) {
            g.lock_holders.remove(&self.object);
        }
        let left = match g.open_locks.get_mut(&self.object) {
            Some(n) => {
                *n -= 1;
                *n
            }
            None => 0,
        };
        if left == 0 {
            g.open_locks.remove(&self.object);
            // The last handle closed: a delete-pending name goes now.
            if let Ok(path) = FakeLock::path(&g, self.object)
                && g.pending.remove(&path)
            {
                purge(&mut g, &path);
            }
        }
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

    /// Claims across every store created from this fake.
    pub fn claim_count(&self) -> usize {
        let g = self.inner.lock().unwrap();
        g.claim_stores.iter().map(|m| m.lock().unwrap().len()).sum()
    }

    /// The claim at `(parent, name)` in any store created from this fake.
    pub fn claim(&self, parent: flux_fs::ObjectId, name: &str) -> Option<ClaimRecord> {
        let k = ClaimKey::new(parent, OsStr::new(name)).encode();
        let g = self.inner.lock().unwrap();
        g.claim_stores
            .iter()
            .find_map(|m| m.lock().unwrap().get(&k).and_then(|v| ClaimRecord::decode(v)))
    }

    pub fn calls(&self) -> Vec<String> {
        self.inner.lock().unwrap().calls.clone()
    }

    pub fn called(&self, prefix: &str) -> bool {
        self.calls().iter().any(|c| c.starts_with(prefix))
    }

    /// What every `lock_capability` call answers from now on (default `LocalStrong`).
    pub fn set_lock_capability(&self, capability: LockCapability) {
        self.inner.lock().unwrap().lock_capability = Some(capability);
    }

    /// Run `action` just before the `nth` call to `name` (1-based, counted like `fail_nth`).
    pub fn on_nth(&self, name: &str, nth: u32, action: impl FnOnce(&FaultFs) + Send + 'static) {
        self.inner.lock().unwrap().hooks.insert(name.to_string(), (nth, Box::new(action)));
    }

    /// Windows' legacy delete semantics (a volume without POSIX delete; cut 7a Part 1 capstone). A file removed while
    /// a lock handle on it is open stays at its name, "delete pending", until the last such handle closes. Meanwhile a
    /// create at the name finds it occupied, and a stat, an open or a read finds nothing.
    pub fn set_legacy_delete(&self, on: bool) {
        self.inner.lock().unwrap().legacy_delete = on;
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

    /// What `mount_root` answers for the directory at `path` (default for every directory: `No`).
    pub fn set_mount_root(&self, path: impl AsRef<Path>, answer: MountRoot) {
        let path = path.as_ref();
        self.inner.lock().unwrap().mount_roots.insert(path.to_path_buf(), answer);
    }

    /// What `canonical_path` answers for the directory at `path` (default: the directory's own snapshot path).
    pub fn set_canonical_path(&self, path: impl AsRef<Path>, canonical: impl AsRef<Path>) {
        let (path, canonical) = (path.as_ref(), canonical.as_ref());
        self.inner.lock().unwrap().canonical.insert(path.to_path_buf(), canonical.to_path_buf());
    }

    /// Make `canonical_path` fail EVERY time on the directory whose snapshot path is `path`, and only there.
    /// The global `fail`/`fail_kind` fault is consumed by whichever handle asks first, so it cannot aim at the
    /// second of two queries; this one can.
    pub fn fail_canonical_path_of(
        &self,
        path: impl AsRef<Path>,
        code: Code,
        kind: std::io::ErrorKind,
    ) {
        self.inner
            .lock()
            .unwrap()
            .canonical_faults
            .insert(path.as_ref().to_path_buf(), (code, kind));
    }

    /// Whether this fake's `rename_no_replace` has an atomic no-replace primitive.
    /// Defaults to `true`; set `false` to make it report the platform's unsupported
    /// error, which is how a caller's behaviour on such a destination is tested
    /// without a filesystem that genuinely lacks one.
    pub fn set_no_replace_support(&self, supported: bool) {
        self.inner.lock().unwrap().no_replace_support = Some(supported);
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

    /// Rebind the NAME at `name` (e.g. `/dst/target`) to whatever `new_target` (e.g.
    /// `/elsewhere`) currently denotes, WITHOUT touching the node any handle already
    /// holds for the old binding. This is item 114's attacker, staged without a
    /// kernel: it can only change what a NAME means for the NEXT lookup, never reach
    /// into a handle that already resolved that name to an id -- exactly the
    /// property a real directory handle has and a path does not. Exists only for
    /// `a_handle_still_addresses_its_directory_after_the_name_is_repointed`.
    pub fn repoint_for_test(&self, name: impl AsRef<Path>, new_target: impl AsRef<Path>) {
        let name = name.as_ref();
        let new_target = new_target.as_ref();
        let (Some(parent_path), Some(child_name)) = (name.parent(), name.file_name()) else {
            return;
        };
        let mut g = self.inner.lock().unwrap();
        let parent_id = dir_node_for_path(&mut g, parent_path);
        let target_id = dir_node_for_path(&mut g, new_target);
        g.dir_nodes
            .get_mut(&parent_id)
            .unwrap()
            .children
            .insert(child_name.to_os_string(), target_id);
    }

    fn record(&self, call: String, key: &str) -> Result<()> {
        let mut g = self.inner.lock().unwrap();
        g.calls.push(call);
        let n = {
            let c = g.call_counts.entry(key.to_string()).or_insert(0);
            *c += 1;
            *c
        };
        let hook = match g.hooks.get(key) {
            Some((at, _)) if *at == n => g.hooks.remove(key),
            _ => None,
        };
        if let Some((_, action)) = hook {
            // Released first: the action calls back into the fake, and `std::sync::Mutex` is not reentrant.
            drop(g);
            action(&FaultFs { inner: std::sync::Arc::clone(&self.inner) });
            g = self.inner.lock().unwrap();
        }
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
        // A DIRECTORY occupies the name too, and this used to check only `files`.
        // MEASURED: `create_new` on a name held by a directory returned Ok and the
        // fake created a file over it, while both real arms refuse with
        // AlreadyExists -- std's `create_new` by path, and the handle arms, which
        // were fixed for exactly this case. The fake was the only implementation
        // that let it through, and it is the one the engine's tests will run
        // against.
        if g.files.contains_key(&p) || g.directories.contains(&p) {
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
        if g.pending.contains(&p) {
            return Err(FsError::new(
                Code::IoError,
                std::io::Error::from(std::io::ErrorKind::NotFound),
            ));
        }
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
        assert!(
            !g.directories.contains(&f),
            "the fake moves a directory only through rename_no_replace (cut 7a Part 3a, decision 10)"
        );
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
        // Checked BEFORE the occupancy test, deliberately: a filesystem that cannot
        // refuse-on-replace cannot answer the occupancy question atomically either,
        // so reporting AlreadyExists here would claim a guarantee this fake is
        // modelling the absence of.
        if g.no_replace_support == Some(false) {
            return Err(FsError::new(
                Code::IoError,
                std::io::Error::new(
                    std::io::ErrorKind::Unsupported,
                    "no atomic no-replace publication primitive",
                ),
            ));
        }
        if g.files.contains_key(&t) || g.directories.contains(&t) {
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
        if g.pending.contains(&p) || !g.files.contains_key(&p) {
            return Err(FsError::new(
                Code::IoError,
                std::io::Error::from(std::io::ErrorKind::NotFound),
            ));
        }
        let object = match g.identities.get(&p) {
            Some(flux_fs::FileIdentity::Strong(o)) => Some(o.index),
            _ => None,
        };
        if g.legacy_delete && object.is_some_and(|o| g.open_locks.get(&o).is_some_and(|&n| n > 0)) {
            g.pending.insert(p);
            return Ok(());
        }
        // The object is gone, so its state goes with it: a later file at the SAME path must not inherit a dead
        // object's identity, permissions or type (each MEASURED as a leak before it was removed here).
        purge(&mut g, &p);
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
        // The fake models a ROOTED filesystem and has no working directory, so a
        // relative path has no meaning in it. Refusing loudly beats the old silent
        // behaviour - MEASURED: `create_dir("a")` SUCCEEDED while `read_dir("")`
        // failed, leaving a directory detached from any tree, because `"a"`'s parent
        // is `""` and `Path::new("").parent()` is `None`, so the parent-must-exist
        // check was skipped.
        //
        // `has_root()`, NOT `is_absolute()`. MEASURED on Windows:
        // `Path::new("/r").is_absolute()` is FALSE, because an absolute path there
        // needs a drive or UNC prefix - so `is_absolute` would reject every path the
        // tests use. `has_root()` is true for `/r`, `/r/a` and `C:\dir`, false for
        // `a`, `a/b` and `""`, which is exactly the distinction wanted.
        assert!(
            path.has_root(),
            "FaultFs models a rooted filesystem and has no working directory, so a \
             relative path cannot be created in it: {path:?}"
        );
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
        // A parent that has NO parent of its own IS a root, so a create directly
        // under it needs no existing entry. `parent != Path::new("/")` was the naive
        // form and it is wrong on Windows, where a drive is a root too -- MEASURED:
        // `Path::new(r"C:\dir").parent()` is `Some(r"C:\")`, which is not `/`, so the
        // old guard fired and rejected a valid root-level create. Asking whether the
        // parent is its own root covers `/`, a drive, a UNC share and the empty
        // relative parent without naming a separator at all.
        if let Some(parent) = p.parent()
            && parent.parent().is_some()
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

impl DestinationRoot for FaultFs {
    type Dir = FakeDirHandle;

    fn destination_root(&self, path: &Path) -> Result<Self::Dir> {
        let mut g = self.inner.lock().unwrap();
        // A path that has a root but no parent IS a root (`/`, `C:\`), which exists on
        // any real filesystem. Same rule `create_dir` applies to a root-level create.
        let is_root = path.has_root() && path.parent().is_none();
        if !is_root && !g.directories.contains(path) {
            // A path that EXISTS but is not a directory is refused as
            // NotADirectory, not NotFound. MEASURED before this: the fake said
            // NotFound for both, while both real arms distinguish them -- and the
            // distinction is the whole point of the trait's requirement, since a
            // caller that passed a file needs to be told that rather than being
            // told its destination is missing.
            let exists = g.files.contains_key(path);
            let kind = if exists {
                std::io::ErrorKind::NotADirectory
            } else {
                std::io::ErrorKind::NotFound
            };
            return Err(FsError::new(Code::IoError, std::io::Error::from(kind)));
        }
        let id = dir_node_for_path(&mut g, path);
        drop(g);
        Ok(FakeDirHandle { id, inner: std::sync::Arc::clone(&self.inner) })
    }
}

/// A handle onto one of the fake's directories, addressed by an id into
/// `Inner::dir_nodes` -- never by the path it was opened through. See `DirNode`.
pub struct FakeDirHandle {
    id: u64,
    inner: std::sync::Arc<Mutex<Inner>>,
}

// Manual, not derived: `Inner` holds a `Mutex` and is not `Debug`, and the id alone
// is all `unwrap_err()` (used on `Result<FakeDirHandle, _>` in the tests) needs.
impl std::fmt::Debug for FakeDirHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FakeDirHandle").field("id", &self.id).finish()
    }
}

impl FakeDirHandle {
    /// A fresh `FaultFs` sharing this handle's state, so a `DirHandle` operation can
    /// be implemented by delegating to the matching `FileSystem` method on a full
    /// path -- reusing its fault injection, call log and identity bookkeeping rather
    /// than duplicating it.
    fn fs(&self) -> FaultFs {
        FaultFs { inner: std::sync::Arc::clone(&self.inner) }
    }

    /// This node's own snapshot path. Fixed at the moment this id was minted; never
    /// re-derived by walking a name.
    fn my_path(&self) -> PathBuf {
        let g = self.inner.lock().unwrap();
        g.dir_nodes
            .get(&self.id)
            .expect("a live FakeDirHandle always names a node still in the graph")
            .path
            .clone()
    }

    /// A new `FakeLock` handle onto the object now at `path`.
    fn lock_for(&self, path: &Path) -> Result<FakeLock> {
        let mut g = self.inner.lock().unwrap();
        let object = match g.identities.get(path) {
            Some(flux_fs::FileIdentity::Strong(o)) => o.index,
            _ => panic!(
                "no strong identity minted for {}: a creation path skipped mint_identity",
                path.display()
            ),
        };
        g.next_lock_handle += 1;
        *g.open_locks.entry(object).or_insert(0) += 1;
        let handle = g.next_lock_handle;
        Ok(FakeLock { object, handle, inner: std::sync::Arc::clone(&self.inner) })
    }
}

impl DirHandle for FakeDirHandle {
    type Writer = FakeHandle;
    type Lock = FakeLock;
    type Claims = FakeClaimStore;

    fn mount_root(&self, _parent: &Self) -> Result<MountRoot> {
        let path = self.my_path();
        self.fs().record(format!("mount_root({})", path.display()), "mount_root")?;
        let g = self.inner.lock().unwrap();
        Ok(g.mount_roots.get(&path).copied().unwrap_or(MountRoot::No))
    }

    fn canonical_path(&self) -> Result<PathBuf> {
        let path = self.my_path();
        self.fs().record(format!("canonical_path({})", path.display()), "canonical_path")?;
        let g = self.inner.lock().unwrap();
        if let Some(&(code, kind)) = g.canonical_faults.get(&path) {
            return Err(FsError::new(code, std::io::Error::new(kind, "injected")));
        }
        Ok(g.canonical.get(&path).cloned().unwrap_or(path))
    }

    fn identity(&self) -> Result<flux_fs::FileIdentity> {
        let path = self.my_path();
        let mut g = self.inner.lock().unwrap();
        // A root is implicitly present in the fake (`destination_root` and `create_dir`
        // treat it so), so it gets an identity on first ask. Every other directory was
        // minted when it was created, and a missing one is a creation path that skipped
        // `mint_identity` - the same loud failure `metadata` gives.
        if path.has_root() && path.parent().is_none() {
            mint_identity(&mut g, &path);
        }
        Ok(*g.identities.get(&path).unwrap_or_else(|| {
            panic!(
                "no identity minted for {}: a creation path skipped mint_identity",
                path.display()
            )
        }))
    }

    fn open_dir(&self, name: &OsStr) -> Result<Self> {
        check_component(name)?;
        // Reuse the binding this handle's OWN node already has for `name`, if any.
        // This is the map `repoint_for_test` rewrites, and reading it here -- rather
        // than recomputing a path -- is what makes a repoint AFTER this call have no
        // effect on the handle this call already returned.
        {
            let g = self.inner.lock().unwrap();
            if let Some(&child_id) = g.dir_nodes.get(&self.id).and_then(|n| n.children.get(name)) {
                return Ok(Self { id: child_id, inner: std::sync::Arc::clone(&self.inner) });
            }
        }
        let child_path = self.my_path().join(name);
        // A MISSING COMPONENT IS A DESTINATION ERROR, which is what both real arms
        // answer and what their `a_missing_component_is_a_destination_error` tests
        // pin. The fake reported whatever `metadata` reported -- IoError/NotFound --
        // so it was the one implementation disagreeing with a rule the other two
        // are tested against.
        let meta = self.fs().metadata(&child_path).map_err(|e| {
            if e.source.kind() == std::io::ErrorKind::NotFound {
                FsError::new(Code::DestinationError, std::io::Error::from(e.source.kind()))
            } else {
                e
            }
        })?;
        if meta.file_type == FileType::Symlink {
            return Err(FsError::new(
                Code::SafetyRejected,
                std::io::Error::other("refuses to traverse a symlink or other name-surrogate"),
            ));
        }
        if meta.file_type != FileType::Dir {
            return Err(FsError::new(
                Code::DestinationError,
                std::io::Error::other("not a directory"),
            ));
        }
        let mut g = self.inner.lock().unwrap();
        let child_id = dir_node_for_path(&mut g, &child_path);
        g.dir_nodes.get_mut(&self.id).unwrap().children.insert(name.to_os_string(), child_id);
        drop(g);
        Ok(Self { id: child_id, inner: std::sync::Arc::clone(&self.inner) })
    }

    fn create_dir(&self, name: &OsStr) -> Result<Self> {
        check_component(name)?;
        let child_path = self.my_path().join(name);
        self.fs().create_dir(&child_path)?;
        let mut g = self.inner.lock().unwrap();
        let child_id = dir_node_for_path(&mut g, &child_path);
        g.dir_nodes.get_mut(&self.id).unwrap().children.insert(name.to_os_string(), child_id);
        drop(g);
        Ok(Self { id: child_id, inner: std::sync::Arc::clone(&self.inner) })
    }

    fn create_new(&self, name: &OsStr) -> Result<Self::Writer> {
        check_component(name)?;
        let child_path = self.my_path().join(name);
        self.fs().create_new(&child_path)
    }

    fn metadata(&self, name: &OsStr) -> Result<Metadata> {
        check_component(name)?;
        let child_path = self.my_path().join(name);
        self.fs().metadata(&child_path)
    }

    fn remove_file(&self, name: &OsStr) -> Result<()> {
        check_component(name)?;
        let child_path = self.my_path().join(name);
        // REFUSE A DIRECTORY, with the kind both real arms use. MEASURED before
        // this: the fake answered NotFound here -- it refused, but only because it
        // found no FILE at the name, never because the object was a directory.
        // Both real arms answer IsADirectory. A fake that refuses for a different
        // reason than reality teaches the engine the wrong branch, which is the
        // same trap as the no-replace kind recorded a few tests below.
        {
            let g = self.inner.lock().unwrap();
            let is_dir = g.directories.contains(&child_path)
                && g.types.get(&child_path) != Some(&FileType::Symlink);
            if is_dir {
                return Err(FsError::new(
                    Code::IoError,
                    std::io::Error::new(
                        std::io::ErrorKind::IsADirectory,
                        "remove_file refuses a directory",
                    ),
                ));
            }
        }
        self.fs().remove_file(&child_path)
    }

    fn rename_no_replace(&self, from: &OsStr, other: &Self, to: &OsStr) -> Result<()> {
        check_component(from)?;
        check_component(to)?;
        let from_path = self.my_path().join(from);
        let to_path = other.my_path().join(to);
        self.fs().rename_no_replace(&from_path, &to_path)
    }

    fn rename_replace(&self, from: &OsStr, other: &Self, to: &OsStr) -> Result<()> {
        check_component(from)?;
        check_component(to)?;
        let from_path = self.my_path().join(from);
        let to_path = other.my_path().join(to);
        self.fs().rename_replace(&from_path, &to_path)
    }

    fn create_lock(&self, name: &OsStr) -> Result<Self::Lock> {
        check_component(name)?;
        let child_path = self.my_path().join(name);
        self.fs().record(format!("create_lock({})", child_path.display()), "create_lock")?;
        {
            // Not through `create_new`: that would count as a `create_new` call for nth-fault injection and consume
            // a pending `write_fault` meant for a data file. The refusal is the same: a file OR a directory holds the
            // name (the reason is recorded in `create_new`).
            let mut g = self.inner.lock().unwrap();
            if g.files.contains_key(&child_path) || g.directories.contains(&child_path) {
                return Err(FsError::new(
                    Code::IoError,
                    std::io::Error::from(std::io::ErrorKind::AlreadyExists),
                ));
            }
            g.files.insert(child_path.clone(), Vec::new());
            mint_identity(&mut g, &child_path);
        }
        self.lock_for(&child_path)
    }

    fn create_claim_store(&self, name: &OsStr, _durability: Durability) -> Result<Self::Claims> {
        check_component(name)?;
        let child_path = self.my_path().join(name);
        self.fs().record(
            format!("create_claim_store({})", child_path.display()),
            "create_claim_store",
        )?;
        let mut g = self.inner.lock().unwrap();
        if g.files.contains_key(&child_path) || g.directories.contains(&child_path) {
            return Err(FsError::new(
                Code::IoError,
                std::io::Error::from(std::io::ErrorKind::AlreadyExists),
            ));
        }
        g.files.insert(child_path.clone(), Vec::new());
        mint_identity(&mut g, &child_path);
        let map = ClaimMap::default();
        g.claim_stores.push(std::sync::Arc::clone(&map));
        Ok(FakeClaimStore { map, inner: std::sync::Arc::clone(&self.inner) })
    }

    fn open_lock(&self, name: &OsStr) -> Result<Self::Lock> {
        check_component(name)?;
        let child_path = self.my_path().join(name);
        self.fs().record(format!("open_lock({})", child_path.display()), "open_lock")?;
        {
            let g = self.inner.lock().unwrap();
            if g.pending.contains(&child_path) {
                return Err(FsError::new(
                    Code::IoError,
                    std::io::Error::from(std::io::ErrorKind::NotFound),
                ));
            }
            match g.types.get(&child_path) {
                Some(FileType::Symlink) => {
                    return Err(FsError::new(
                        Code::SafetyRejected,
                        std::io::Error::other(
                            "refuses to follow a symlink or other name-surrogate",
                        ),
                    ));
                }
                Some(FileType::Other) => {
                    return Err(FsError::new(
                        Code::DestinationError,
                        std::io::Error::other(
                            "a lock path holds something other than a regular file",
                        ),
                    ));
                }
                _ => {}
            }
            if g.directories.contains(&child_path) {
                return Err(FsError::new(
                    Code::DestinationError,
                    std::io::Error::new(
                        std::io::ErrorKind::IsADirectory,
                        "a directory is not a lock file",
                    ),
                ));
            }
            if !g.files.contains_key(&child_path) {
                return Err(FsError::new(
                    Code::IoError,
                    std::io::Error::from(std::io::ErrorKind::NotFound),
                ));
            }
        }
        self.lock_for(&child_path)
    }

    fn lock_capability(&self) -> Result<LockCapability> {
        self.fs().record("lock_capability()".to_string(), "lock_capability")?;
        Ok(self.inner.lock().unwrap().lock_capability.unwrap_or(LockCapability::LocalStrong))
    }

    fn read_dir(&self) -> Result<Vec<DirEntry>> {
        self.fs().read_dir(&self.my_path())
    }

    fn read_file(&self, name: &OsStr, limit: usize) -> Result<Vec<u8>> {
        check_component(name)?;
        let child_path = self.my_path().join(name);
        self.fs().record(format!("read_file({})", child_path.display()), "read_file")?;
        let g = self.inner.lock().unwrap();
        if g.pending.contains(&child_path) {
            return Err(FsError::new(
                Code::IoError,
                std::io::Error::from(std::io::ErrorKind::NotFound),
            ));
        }
        match g.types.get(&child_path) {
            Some(FileType::Symlink) => {
                return Err(FsError::new(
                    Code::SafetyRejected,
                    std::io::Error::other("refuses to follow a symlink or other name-surrogate"),
                ));
            }
            Some(FileType::Other) => {
                return Err(FsError::new(
                    Code::DestinationError,
                    std::io::Error::other("not a regular file"),
                ));
            }
            _ => {}
        }
        if g.directories.contains(&child_path) {
            return Err(FsError::new(
                Code::DestinationError,
                std::io::Error::new(
                    std::io::ErrorKind::IsADirectory,
                    "a directory is not a state file",
                ),
            ));
        }
        let Some(bytes) = g.files.get(&child_path) else {
            return Err(FsError::new(
                Code::IoError,
                std::io::Error::from(std::io::ErrorKind::NotFound),
            ));
        };
        let mut bytes = bytes.clone();
        bytes.truncate(limit + 1);
        Ok(bytes)
    }

    fn remove_dir(&self, name: &OsStr) -> Result<()> {
        check_component(name)?;
        let child_path = self.my_path().join(name);
        self.fs().record(format!("remove_dir({})", child_path.display()), "remove_dir")?;
        let mut g = self.inner.lock().unwrap();
        // A link (of any kind) or a file: as `unlinkat(AT_REMOVEDIR)` answers, never followed.
        if g.types.get(&child_path) == Some(&FileType::Symlink) || g.files.contains_key(&child_path)
        {
            return Err(FsError::new(
                Code::IoError,
                std::io::Error::new(
                    std::io::ErrorKind::NotADirectory,
                    "remove_dir refuses what is not a directory",
                ),
            ));
        }
        if !g.directories.contains(&child_path) {
            return Err(FsError::new(
                Code::IoError,
                std::io::Error::from(std::io::ErrorKind::NotFound),
            ));
        }
        let occupied = g
            .files
            .keys()
            .chain(g.directories.iter())
            .any(|k| k.parent() == Some(child_path.as_path()));
        if occupied {
            return Err(FsError::new(
                Code::IoError,
                std::io::Error::from(std::io::ErrorKind::DirectoryNotEmpty),
            ));
        }
        g.directories.remove(&child_path);
        g.types.remove(&child_path);
        g.identities.remove(&child_path);
        g.times.remove(&child_path);
        g.perms.remove(&child_path);
        // The name no longer binds a node: a directory made there later is a new one.
        g.dir_node_by_path.remove(&child_path);
        if let Some(node) = g.dir_nodes.get_mut(&self.id) {
            node.children.remove(name);
        }
        Ok(())
    }

    fn sync(&self) -> Result<()> {
        self.fs().record(format!("sync_dir({})", self.my_path().display()), "sync_dir")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flux_fs::FileIdentity;

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
    fn canonical_path_is_the_handles_own_path_unless_a_test_set_one() {
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/d")).unwrap();
        fs.create_dir(Path::new("/d/x")).unwrap();
        fs.set_canonical_path("/d/x", "/elsewhere/x");
        let d = fs.destination_root(Path::new("/d")).unwrap();
        let x = d.open_dir(OsStr::new("x")).unwrap();
        assert_eq!(d.canonical_path().unwrap(), PathBuf::from("/d"));
        assert_eq!(x.canonical_path().unwrap(), PathBuf::from("/elsewhere/x"));
    }

    #[test]
    fn canonical_path_takes_an_injected_fault_with_its_kind() {
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/d")).unwrap();
        fs.fail_kind("canonical_path", Code::IoError, std::io::ErrorKind::Unsupported);
        let d = fs.destination_root(Path::new("/d")).unwrap();
        let e = d.canonical_path().unwrap_err();
        assert_eq!(e.source.kind(), std::io::ErrorKind::Unsupported);
        assert_eq!(d.canonical_path().unwrap(), PathBuf::from("/d"), "consumed on use");
    }

    #[test]
    fn a_per_directory_canonical_path_fault_hits_only_that_directory_and_every_time() {
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/d")).unwrap();
        fs.create_dir(Path::new("/d/x")).unwrap();
        fs.fail_canonical_path_of(
            "/d/x",
            Code::PermissionDenied,
            std::io::ErrorKind::PermissionDenied,
        );
        let d = fs.destination_root(Path::new("/d")).unwrap();
        let x = d.open_dir(OsStr::new("x")).unwrap();
        assert_eq!(d.canonical_path().unwrap(), PathBuf::from("/d"));
        for _ in 0..2 {
            let e = x.canonical_path().unwrap_err();
            assert_eq!(
                (e.code, e.source.kind()),
                (Code::PermissionDenied, std::io::ErrorKind::PermissionDenied)
            );
        }
    }

    #[test]
    fn mount_root_answers_no_by_default_and_what_a_test_set() {
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/d")).unwrap();
        fs.create_dir(Path::new("/d/m")).unwrap();
        fs.create_dir(Path::new("/d/n")).unwrap();
        fs.set_mount_root("/d/m", MountRoot::Yes);
        let d = fs.destination_root(Path::new("/d")).unwrap();
        let m = d.open_dir(OsStr::new("m")).unwrap();
        let n = d.open_dir(OsStr::new("n")).unwrap();
        assert_eq!(m.mount_root(&d).unwrap(), MountRoot::Yes);
        assert_eq!(n.mount_root(&d).unwrap(), MountRoot::No);
        assert!(
            fs.calls().iter().any(|c| c.replace('\\', "/") == "mount_root(/d/m)"),
            "{:?}",
            fs.calls()
        );
    }

    #[test]
    fn mount_root_takes_an_injected_fault() {
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/d")).unwrap();
        fs.create_dir(Path::new("/d/m")).unwrap();
        fs.fail("mount_root", Code::PermissionDenied);
        let d = fs.destination_root(Path::new("/d")).unwrap();
        let m = d.open_dir(OsStr::new("m")).unwrap();
        assert_eq!(m.mount_root(&d).unwrap_err().code, Code::PermissionDenied);
        assert_eq!(m.mount_root(&d).unwrap(), MountRoot::No, "consumed on use");
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

    #[cfg(windows)]
    #[test]
    fn create_dir_allows_a_create_directly_under_a_windows_drive_root() {
        // The guard used to compare the parent against `/` alone, so a drive root was
        // not recognised as a root and this was rejected with NotFound. MEASURED:
        // `Path::new(r"C:\dir").parent()` is `Some(r"C:\\")`, which is not `/`.
        // Nothing touches a real disk here - this is the fake's own namespace.
        let fs = FaultFs::new();
        fs.create_dir(Path::new(r"C:\flux_fake_root")).unwrap();
        assert_eq!(fs.metadata(Path::new(r"C:\flux_fake_root")).unwrap().file_type, FileType::Dir);
    }

    #[test]
    fn create_dir_still_requires_a_real_parent_below_the_root() {
        // The other half of the root fix: relaxing the root case must not relax the
        // ordinary case, or the walk could create a directory whose parent is absent.
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/a")).unwrap();
        fs.create_dir(Path::new("/a/b")).unwrap();
        assert!(fs.create_dir(Path::new("/a/missing/c")).is_err());
    }

    #[test]
    #[should_panic(expected = "relative path cannot be created")]
    fn create_dir_refuses_a_relative_path_rather_than_detaching_it() {
        // Before this guard, `create_dir("a")` SUCCEEDED while `read_dir("")` failed:
        // "a"'s parent is "", and `Path::new("").parent()` is `None`, so the
        // parent-must-exist check was skipped and the directory ended up attached to
        // a root that can never exist.
        let fs = FaultFs::new();
        let _ = fs.create_dir(Path::new("a"));
    }

    #[test]
    fn create_dir_still_accepts_every_rooted_form() {
        // The guard is `has_root()`, not `is_absolute()`. MEASURED on Windows:
        // `Path::new("/r").is_absolute()` is FALSE, so `is_absolute` would have
        // rejected every path these tests use.
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/r")).unwrap();
        fs.create_dir(Path::new("/r/a")).unwrap();
    }

    #[test]
    fn no_replace_support_is_on_by_default() {
        // Every existing test was written before this switch existed and must keep
        // passing untouched, which is only true if the default is the old behaviour.
        let fs = FaultFs::new();
        fs.write_file("/from", b"new");
        assert!(fs.rename_no_replace(Path::new("/from"), Path::new("/to")).is_ok());
    }

    #[test]
    fn a_destination_without_the_primitive_reports_it() {
        let fs = FaultFs::new();
        fs.write_file("/from", b"new");
        fs.set_no_replace_support(false);

        let err = fs.rename_no_replace(Path::new("/from"), Path::new("/to")).unwrap_err();

        // NOT AlreadyExists: the name is free. The filesystem cannot make the promise
        // at all, which is a different fact.
        //
        // THE KIND BELOW IS THIS FAKE'S CHOICE, NOT A CONTRACT, and an earlier version
        // of this comment said it was "the one the engine cut branches on" -- which
        // would be a trap if the engine took it literally. MEASURED on a real
        // 9p-mounted volume: `rename_no_replace` there fails with EINVAL, so the kind
        // is `InvalidInput`, NOT `Unsupported`. A third filesystem may well answer a
        // third way.
        //
        // So the engine's §241.5 probe must treat ANY failure of an attempted
        // no-replace publish as "the primitive is unavailable", and must not match on
        // a particular `ErrorKind`. An engine written to pass against this fake by
        // checking for `Unsupported` alone would pass its tests and miss the real
        // case on Linux -- the exact inversion of what a fake is for. Tracked with
        // the rest of the item-113 debt.
        assert_eq!(err.code, Code::IoError);
        assert_eq!(err.source.kind(), std::io::ErrorKind::Unsupported);
        assert!(!fs.exists("/to"), "an unsupported primitive must not fall back to a plain rename");
        assert!(fs.exists("/from"), "the source must be untouched");
    }

    #[test]
    fn a_handle_still_addresses_its_directory_after_the_name_is_repointed() {
        // Item 114's attack, staged without a kernel. The fake's handle is an id
        // into its tree rather than a name, so repointing the NAME leaves the
        // handle addressing the ORIGINAL node -- which is exactly what a real
        // directory handle does, and exactly what a path does not.
        //
        // WHAT THIS PROVES, AND WHAT IT DOES NOT. Stated because an earlier reading
        // of it claimed more, and the gap is the kind that looks like coverage.
        //
        // PROVES: the handle does not RE-RESOLVE its name at use time. MEASURED by
        // mutation -- making `my_path` re-resolve this node's own last component
        // through the parent's (repointable) `children` binding on every call reds
        // this test, with this message, while its sibling stays green.
        //
        // DOES NOT PROVE: that a path-holding handle is refused. A `FakeDirHandle`
        // storing a `PathBuf` snapshot taken at open time would also pass, because
        // `Inner`'s maps are keyed flat by `PathBuf` and `repoint_for_test` rewrites
        // only the `children` bindings -- so nothing re-aliases the stored path and
        // the write lands where it always would. In the REAL arms that distinction
        // does not exist: the kernel re-resolves a stored path on every syscall, so
        // holding one IS the defect. The fake cannot express that yet.
        //
        // Closing it means re-keying this fake's storage by node id so its
        // path-based methods traverse, and that is deliberately NOT done here. It is
        // not a data-structure swap: `write_file` currently creates a file at any
        // depth with no parent, `move_object` re-keys only the exact string (a
        // renamed directory strands its children), and a symlink carries no target
        // at all -- so a faithful double needs hierarchical strictness, descendant
        // re-keying and a resolution engine with cycle limits. The consumer that
        // fixes those requirements is cut 4's `copy_tree`, which does not exist yet;
        // building them now would be designing against an imagined caller. The peer
        // reviewing this first argued for re-keying immediately and reversed after
        // measuring all three points above; the agreed disposition is to document
        // the limit here and land the re-key in cut 4, where its caller defines it.
        use flux_fs::{DestinationRoot, DirHandle};
        use std::ffi::OsStr;

        let fs = FaultFs::new();
        fs.create_dir(Path::new("/dst")).unwrap();
        fs.create_dir(Path::new("/dst/target")).unwrap();
        fs.create_dir(Path::new("/elsewhere")).unwrap();

        let root = fs.destination_root(Path::new("/dst")).unwrap();
        let held = root.open_dir(OsStr::new("target")).unwrap();

        // The attacker swaps what the NAME means.
        fs.repoint_for_test(Path::new("/dst/target"), Path::new("/elsewhere"));

        held.create_new(OsStr::new("payload")).unwrap();

        assert!(
            !fs.exists("/elsewhere/payload"),
            "the write followed the swapped name; the handle was not load-bearing"
        );
    }

    #[test]
    fn the_fake_refuses_a_name_with_a_separator() {
        use flux_fs::{DestinationRoot, DirHandle};
        use std::ffi::OsStr;

        let fs = FaultFs::new();
        fs.create_dir(Path::new("/dst")).unwrap();
        let root = fs.destination_root(Path::new("/dst")).unwrap();
        let err = root.open_dir(OsStr::new("a/b")).unwrap_err();
        assert_eq!(err.code, Code::SafetyRejected);
    }

    #[test]
    fn the_fake_refuses_what_the_real_arms_refuse_and_says_the_same_thing() {
        // A fake is only useful if it is wrong in the same places reality is. These
        // three were MEASURED to diverge: the fake refused all of them, but for
        // different reasons and with different kinds than the POSIX and Windows
        // arms, so an engine written against it would learn the wrong branch and
        // meet the real kind in production. The trait now states each of these as a
        // requirement; this is what holds the fake to it.
        use flux_fs::{DestinationRoot, DirHandle};
        use std::ffi::OsStr;

        let fs = FaultFs::new();
        fs.create_dir(Path::new("/dst")).unwrap();
        fs.create_dir(Path::new("/dst/adir")).unwrap();
        fs.write_file("/dst/afile", b"x");
        let root = fs.destination_root(Path::new("/dst")).unwrap();

        // remove_file must refuse a DIRECTORY as IsADirectory, not NotFound.
        let err = root.remove_file(OsStr::new("adir")).unwrap_err();
        assert_eq!(err.source.kind(), std::io::ErrorKind::IsADirectory);
        assert!(fs.exists("/dst/adir"), "the directory must survive the refusal");

        // create_dir must refuse an occupied name as AlreadyExists.
        let err = match root.create_dir(OsStr::new("adir")) {
            Err(e) => e,
            Ok(_) => panic!("create_dir must refuse an occupied name"),
        };
        assert_eq!(err.source.kind(), std::io::ErrorKind::AlreadyExists);

        // A FILE as the destination root is NotADirectory; a MISSING one is
        // NotFound. The fake used to answer NotFound to both.
        let err = match fs.destination_root(Path::new("/dst/afile")) {
            Err(e) => e,
            Ok(_) => panic!("a file must not be accepted as a destination root"),
        };
        assert_eq!(err.source.kind(), std::io::ErrorKind::NotADirectory);
        let err = match fs.destination_root(Path::new("/dst/nosuch")) {
            Err(e) => e,
            Ok(_) => panic!("a missing root must be refused"),
        };
        assert_eq!(err.source.kind(), std::io::ErrorKind::NotFound);

        // create_new must see a DIRECTORY as occupying the name. MEASURED before
        // this: the fake returned Ok and created a file over it, alone among the
        // three implementations.
        let err = match root.create_new(OsStr::new("adir")) {
            Err(e) => e,
            Ok(_) => panic!("a name a directory holds is taken"),
        };
        assert_eq!(err.source.kind(), std::io::ErrorKind::AlreadyExists);
        assert!(fs.exists("/dst/adir"), "the directory must survive");

        // A missing component is a DESTINATION error, which is the rule both real
        // arms are tested against. The fake used to pass through metadata's
        // IoError/NotFound.
        let err = match root.open_dir(OsStr::new("absent")) {
            Err(e) => e,
            Ok(_) => panic!("a missing component must be refused"),
        };
        assert_eq!(err.code, Code::DestinationError);
    }

    #[test]
    fn destination_root_accepts_the_filesystem_root() {
        // A real filesystem always has its root, and create_dir already treats a
        // root as present (the root-parent rule in `create_dir`). copy_file opens
        // `/dst`'s parent through destination_root, so the fake must agree.
        use flux_fs::{DestinationRoot, DirHandle};
        let fs = FaultFs::new();
        let root = fs.destination_root(Path::new("/")).expect("the root is a directory");
        let mut w = root.create_new(std::ffi::OsStr::new("f")).unwrap();
        std::io::Write::write_all(&mut w, b"x").unwrap();
        drop(w);
        assert_eq!(fs.read_file("/f").as_deref(), Some(&b"x"[..]));
    }

    #[test]
    fn a_fake_handle_reports_the_identity_of_its_directory() {
        use flux_fs::{DestinationRoot, DirHandle, FileSystem};
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/d")).unwrap();
        let root = fs.destination_root(Path::new("/")).unwrap();
        let d = fs.destination_root(Path::new("/d")).unwrap();

        assert!(matches!(root.identity().unwrap(), flux_fs::FileIdentity::Strong(_)));
        assert_eq!(d.identity().unwrap(), fs.metadata(Path::new("/d")).unwrap().identity);
        assert_ne!(root.identity().unwrap(), d.identity().unwrap());
        // Asking is not a filesystem call: it must not shift `fail_nth` counts.
        let calls = fs.calls().len();
        let _ = d.identity().unwrap();
        assert_eq!(fs.calls().len(), calls);
    }
    #[test]
    fn a_fake_lock_is_exclusive_per_object_and_released_on_drop() {
        use flux_fs::{DestinationRoot, DirHandle, FileSystem, LockFile};
        use std::ffi::OsStr;
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/d")).unwrap();
        let d = fs.destination_root(Path::new("/d")).unwrap();
        let first = d.create_lock(OsStr::new("x.flux-lock")).unwrap();
        assert_eq!(
            d.create_lock(OsStr::new("x.flux-lock")).unwrap_err().source.kind(),
            std::io::ErrorKind::AlreadyExists
        );
        assert!(first.try_lock().unwrap());
        let second = d.open_lock(OsStr::new("x.flux-lock")).unwrap();
        assert!(!second.try_lock().unwrap());
        drop(first);
        assert!(second.try_lock().unwrap());
    }

    #[test]
    fn a_fake_lock_follows_its_object_across_a_rename() {
        use flux_fs::{DestinationRoot, DirHandle, FileSystem, LockFile};
        use std::ffi::OsStr;
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/d")).unwrap();
        let d = fs.destination_root(Path::new("/d")).unwrap();
        let holder = d.create_lock(OsStr::new("x.flux-lock")).unwrap();
        holder.write_at_start(b"record").unwrap();
        assert!(holder.try_lock().unwrap());
        d.rename_no_replace(OsStr::new("x.flux-lock"), &d, OsStr::new("x.flux-lock.broken.1"))
            .unwrap();
        let moved = d.open_lock(OsStr::new("x.flux-lock.broken.1")).unwrap();
        assert!(!moved.try_lock().unwrap(), "the lock moved with the object");
        assert_eq!(moved.read_all(4096).unwrap(), b"record");
        assert_eq!(moved.identity().unwrap(), holder.identity().unwrap());
    }

    #[test]
    fn a_fake_open_lock_refuses_what_the_real_ones_refuse() {
        use flux_fs::{DestinationRoot, DirHandle, FileSystem};
        use std::ffi::OsStr;
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/d")).unwrap();
        let d = fs.destination_root(Path::new("/d")).unwrap();
        let e = d.open_lock(OsStr::new("absent")).unwrap_err();
        assert_eq!((e.code, e.source.kind()), (Code::IoError, std::io::ErrorKind::NotFound));
        fs.write_file("/d/link", b"");
        fs.set_type("/d/link", FileType::Symlink);
        assert_eq!(d.open_lock(OsStr::new("link")).unwrap_err().code, Code::SafetyRejected);
        drop(d.create_dir(OsStr::new("sub")).unwrap());
        let e = d.open_lock(OsStr::new("sub")).unwrap_err();
        assert_eq!(
            (e.code, e.source.kind()),
            (Code::DestinationError, std::io::ErrorKind::IsADirectory)
        );
    }

    #[test]
    fn a_fake_lock_write_replaces_the_contents_and_cuts_the_old_tail() {
        use flux_fs::{DestinationRoot, DirHandle, FileSystem, LockFile};
        use std::ffi::OsStr;
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/d")).unwrap();
        let d = fs.destination_root(Path::new("/d")).unwrap();
        let h = d.create_lock(OsStr::new("x")).unwrap();
        h.write_at_start(&[1u8; 8]).unwrap();
        let before = h.identity().unwrap();
        h.write_at_start(&[2u8; 4]).unwrap();
        assert_eq!(
            h.read_all(100).unwrap(),
            vec![2, 2, 2, 2],
            "the old tail is cut, as on the real platforms"
        );
        assert_eq!(h.read_all(3).unwrap().len(), 4, "limit + 1");
        assert_eq!(h.identity().unwrap(), before, "the same object");
    }

    #[test]
    fn a_fault_at_the_fake_locks_cut_leaves_the_new_record_followed_by_the_old_tail() {
        use flux_fs::{DestinationRoot, DirHandle, FileSystem, LockFile};
        use std::ffi::OsStr;
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/d")).unwrap();
        let d = fs.destination_root(Path::new("/d")).unwrap();
        let h = d.create_lock(OsStr::new("x")).unwrap();
        h.write_at_start(&[1u8; 8]).unwrap();
        fs.fail("lock_set_len", Code::IoError);
        assert!(h.write_at_start(&[2u8; 4]).is_err());
        assert_eq!(
            h.read_all(100).unwrap(),
            vec![2, 2, 2, 2, 1, 1, 1, 1],
            "what a crash between write and cut leaves"
        );
    }

    #[test]
    fn a_removed_name_forgets_the_type_a_test_gave_it() {
        use flux_fs::{DestinationRoot, DirHandle, FileSystem};
        use std::ffi::OsStr;
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/d")).unwrap();
        let d = fs.destination_root(Path::new("/d")).unwrap();
        fs.write_file("/d/x", b"");
        fs.set_type("/d/x", FileType::Symlink);
        fs.remove_file(Path::new("/d/x")).unwrap();
        drop(d.create_lock(OsStr::new("x")).unwrap());
        d.open_lock(OsStr::new("x")).expect("a lock created after the removal is a regular file");
    }

    #[test]
    fn the_fake_answers_the_capability_it_is_told() {
        use flux_fs::{DestinationRoot, DirHandle};
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/d")).unwrap();
        let d = fs.destination_root(Path::new("/d")).unwrap();
        assert_eq!(d.lock_capability().unwrap(), LockCapability::LocalStrong);
        fs.set_lock_capability(LockCapability::Unsupported);
        assert_eq!(d.lock_capability().unwrap(), LockCapability::Unsupported);
    }

    #[test]
    fn a_hook_runs_just_before_the_nth_call_and_can_drive_the_fake() {
        use flux_fs::FileSystem;
        let fs = FaultFs::new();
        fs.on_nth("metadata", 2, |fs| fs.write_file("/b", b"y"));
        assert!(fs.metadata(Path::new("/b")).is_err(), "first call: the hook has not run");
        assert!(fs.metadata(Path::new("/b")).is_ok(), "second call: the hook ran just before it");
        assert!(fs.metadata(Path::new("/b")).is_ok(), "a hook runs once");
    }

    #[test]
    fn a_fake_handle_lists_its_directory() {
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/p")).unwrap();
        let d = fs.destination_root(Path::new("/p")).unwrap();
        fs.write_file("/p/a", b"x");
        let sub = d.create_dir(OsStr::new("sub")).unwrap();
        fs.write_file("/p/sub/deeper", b"y");
        let mut got: Vec<_> =
            d.read_dir().unwrap().into_iter().map(|e| (e.name, e.file_type)).collect();
        got.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(
            got,
            vec![(OsString::from("a"), FileType::File), (OsString::from("sub"), FileType::Dir)]
        );
        assert!(fs.called("read_dir("), "{:?}", fs.calls());
        drop(sub);
    }

    #[test]
    fn a_fake_handle_reads_a_file_and_refuses_what_the_real_ones_refuse() {
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/p")).unwrap();
        let d = fs.destination_root(Path::new("/p")).unwrap();
        fs.write_file("/p/s", b"0123456789");
        assert_eq!(d.read_file(OsStr::new("s"), 4).unwrap(), b"01234");
        let e = d.read_file(OsStr::new("absent"), 4).unwrap_err();
        assert_eq!((e.code, e.source.kind()), (Code::IoError, std::io::ErrorKind::NotFound));
        drop(d.create_dir(OsStr::new("sub")).unwrap());
        let e = d.read_file(OsStr::new("sub"), 4).unwrap_err();
        assert_eq!(
            (e.code, e.source.kind()),
            (Code::DestinationError, std::io::ErrorKind::IsADirectory)
        );
        fs.write_file("/p/l", b"");
        fs.set_type("/p/l", FileType::Symlink);
        assert_eq!(d.read_file(OsStr::new("l"), 4).unwrap_err().code, Code::SafetyRejected);
        assert!(fs.called("read_file("), "{:?}", fs.calls());
    }

    #[test]
    fn a_fake_remove_dir_removes_only_an_empty_real_directory() {
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/p")).unwrap();
        let d = fs.destination_root(Path::new("/p")).unwrap();
        let empty = d.create_dir(OsStr::new("empty")).unwrap();
        let before = fs.metadata(Path::new("/p/empty")).unwrap().identity;
        drop(empty);
        d.remove_dir(OsStr::new("empty")).unwrap();
        assert!(!fs.exists("/p/empty"));
        drop(d.create_dir(OsStr::new("empty")).unwrap());
        assert_ne!(
            fs.metadata(Path::new("/p/empty")).unwrap().identity,
            before,
            "a directory made again at the name is a new object"
        );
        drop(d.create_dir(OsStr::new("full")).unwrap());
        fs.write_file("/p/full/f", b"x");
        let e = d.remove_dir(OsStr::new("full")).unwrap_err();
        assert_eq!(e.source.kind(), std::io::ErrorKind::DirectoryNotEmpty);
        fs.write_file("/p/file", b"x");
        let e = d.remove_dir(OsStr::new("file")).unwrap_err();
        assert_eq!(e.source.kind(), std::io::ErrorKind::NotADirectory);
        drop(d.create_dir(OsStr::new("link")).unwrap());
        fs.set_type("/p/link", FileType::Symlink);
        let e = d.remove_dir(OsStr::new("link")).unwrap_err();
        assert_eq!(e.source.kind(), std::io::ErrorKind::NotADirectory);
        assert!(fs.exists("/p/link"));
        let e = d.remove_dir(OsStr::new("absent")).unwrap_err();
        assert_eq!(e.source.kind(), std::io::ErrorKind::NotFound);
    }

    #[test]
    fn a_fake_sync_is_recorded_and_can_fail() {
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/p")).unwrap();
        let d = fs.destination_root(Path::new("/p")).unwrap();
        d.sync().unwrap();
        assert!(fs.called("sync_dir("), "{:?}", fs.calls());
        fs.fail("sync_dir", Code::IoError);
        assert!(d.sync().is_err());
    }

    #[test]
    fn a_fake_directory_rename_moves_the_subtree_and_open_handles_follow() {
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/p")).unwrap();
        let d = fs.destination_root(Path::new("/p")).unwrap();
        let building = d.create_dir(OsStr::new("w.creating")).unwrap();
        fs.write_file("/p/w.creating/manifest", b"m");
        let dir_id = fs.metadata(Path::new("/p/w.creating")).unwrap().identity;
        let file_id = fs.metadata(Path::new("/p/w.creating/manifest")).unwrap().identity;
        d.rename_no_replace(OsStr::new("w.creating"), &d, OsStr::new("w")).unwrap();
        assert!(!fs.exists("/p/w.creating") && !fs.exists("/p/w.creating/manifest"));
        assert_eq!(fs.read_file("/p/w/manifest").as_deref(), Some(&b"m"[..]));
        assert_eq!(fs.metadata(Path::new("/p/w")).unwrap().identity, dir_id, "the same object");
        assert_eq!(fs.metadata(Path::new("/p/w/manifest")).unwrap().identity, file_id);
        assert_eq!(
            building.read_file(OsStr::new("manifest"), 8).unwrap(),
            b"m",
            "a handle opened before the rename follows the directory"
        );
        let opened = d.open_dir(OsStr::new("w")).unwrap();
        assert_eq!(opened.read_file(OsStr::new("manifest"), 8).unwrap(), b"m");
        assert_eq!(d.open_dir(OsStr::new("w.creating")).unwrap_err().code, Code::DestinationError);
        drop(d.create_dir(OsStr::new("x")).unwrap());
        let e = d.rename_no_replace(OsStr::new("w"), &d, OsStr::new("x")).unwrap_err();
        assert_eq!(e.source.kind(), std::io::ErrorKind::AlreadyExists);
    }

    #[test]
    fn a_fake_directory_rename_carries_types_permissions_and_times() {
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/p")).unwrap();
        let d = fs.destination_root(Path::new("/p")).unwrap();
        drop(d.create_dir(OsStr::new("w.creating")).unwrap());
        fs.add_symlink("/p/w.creating/link");
        fs.write_file("/p/w.creating/f", b"f");
        fs.set_file_perms("/p/w.creating/f", Perms::UnixMode(0o600));
        let t = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(7);
        let w = fs.create_new(Path::new("/p/w.creating/timed")).unwrap();
        fs.set_times(&w, Some(t)).unwrap();
        drop(w);
        d.rename_no_replace(OsStr::new("w.creating"), &d, OsStr::new("w")).unwrap();
        assert_eq!(fs.metadata(Path::new("/p/w/link")).unwrap().file_type, FileType::Symlink);
        assert_eq!(fs.permissions("/p/w/f"), Some(Perms::UnixMode(0o600)));
        assert_eq!(fs.modified("/p/w/timed"), Some(t));
    }

    #[test]
    fn legacy_delete_keeps_a_name_occupied_until_the_last_lock_handle_closes() {
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/p")).unwrap();
        let d = fs.destination_root(Path::new("/p")).unwrap();
        fs.set_legacy_delete(true);
        let name = OsStr::new("x.flux-lock");
        let held = d.create_lock(name).unwrap();
        let other = d.open_lock(name).unwrap();
        d.remove_file(name).unwrap();
        drop(held);
        let gone = |e: FsError| e.source.kind() == std::io::ErrorKind::NotFound;
        assert!(gone(d.metadata(name).unwrap_err()), "a stat finds nothing");
        assert!(gone(d.open_lock(name).unwrap_err()), "an open finds nothing");
        assert!(gone(d.read_file(name, 8).unwrap_err()), "a read finds nothing");
        assert_eq!(
            d.create_lock(name).unwrap_err().source.kind(),
            std::io::ErrorKind::AlreadyExists,
            "a create finds the name occupied"
        );
        drop(other);
        assert!(!fs.exists("/p/x.flux-lock"), "the last close deletes it");
        drop(d.create_lock(name).unwrap());
    }

    #[test]
    fn without_legacy_delete_a_removal_is_immediate_even_while_a_handle_is_open() {
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/p")).unwrap();
        let d = fs.destination_root(Path::new("/p")).unwrap();
        let name = OsStr::new("x.flux-lock");
        let held = d.create_lock(name).unwrap();
        d.remove_file(name).unwrap();
        assert!(!fs.exists("/p/x.flux-lock"));
        drop(d.create_lock(name).unwrap());
        drop(held);
    }

    #[test]
    fn a_fake_writers_identity_is_its_objects_and_a_rename_carries_it() {
        use flux_fs::{DestinationRoot, DirHandle, FileHandle};
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/p")).unwrap();
        let p = fs.destination_root(Path::new("/p")).unwrap();
        let w = p.create_new(OsStr::new("t.tmp")).unwrap();
        let held = w.identity().unwrap();
        assert!(matches!(held, FileIdentity::Strong(_)));
        p.rename_replace(OsStr::new("t.tmp"), &p, OsStr::new("t")).unwrap();
        assert_eq!(fs.metadata(Path::new("/p/t")).unwrap().identity, held);
    }

    #[test]
    fn the_fake_claim_store_passes_the_conformance_suite() {
        use flux_fs::{DestinationRoot, DirHandle, Durability};
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/d")).unwrap();
        let d = fs.destination_root(Path::new("/d")).unwrap();
        let n = std::cell::Cell::new(0u32);
        flux_fs::claims::conformance::run_all(|| {
            n.set(n.get() + 1);
            let name = format!("state{}.db", n.get());
            d.create_claim_store(OsStr::new(&name), Durability::Normal).unwrap()
        });
    }

    #[test]
    fn create_claim_store_refuses_a_taken_name() {
        use flux_fs::{DestinationRoot, DirHandle, Durability};
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/d")).unwrap();
        let d = fs.destination_root(Path::new("/d")).unwrap();
        let _first = d.create_claim_store(OsStr::new("state.db"), Durability::Normal).unwrap();
        assert!(fs.exists("/d/state.db"), "an empty file entry is visible at the path");
        assert!(
            fs.calls().iter().any(|c| c.replace('\\', "/") == "create_claim_store(/d/state.db)"),
            "the call log names the path: {:?}",
            fs.calls()
        );
        let err = match d.create_claim_store(OsStr::new("state.db"), Durability::Normal) {
            Err(e) => e,
            Ok(_) => panic!("a taken name must be refused"),
        };
        assert_eq!(err.source.kind(), std::io::ErrorKind::AlreadyExists);
    }

    #[test]
    fn a_claim_fault_is_injected_and_consumed() {
        use flux_fs::{
            ClaimKey, ClaimRecord, ClaimStatus, ClaimStore, DestinationRoot, DirHandle, Durability,
            FluxPathKey, ObjectId,
        };
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/d")).unwrap();
        let d = fs.destination_root(Path::new("/d")).unwrap();
        let mut s = d.create_claim_store(OsStr::new("state.db"), Durability::Normal).unwrap();
        let key = ClaimKey::new(ObjectId { volume: 1, index: 1 }, OsStr::new("a"));
        let rec = ClaimRecord { target: FluxPathKey(b"a".to_vec()), status: ClaimStatus::Existing };
        fs.fail("claim_insert", Code::IoError);
        assert!(s.insert_if_absent(&key, &rec).is_err());
        assert!(s.insert_if_absent(&key, &rec).is_ok(), "the fault is consumed");
        assert!(fs.called("claim_insert(a)"));
        fs.fail("claim_upgrade", Code::IoError);
        assert!(s.upgrade_own_claim(&key, &rec.target).is_err());
        assert!(s.upgrade_own_claim(&key, &rec.target).is_ok());
        assert!(fs.called("claim_upgrade(a)"));
        fs.fail("claim_flush", Code::IoError);
        assert!(s.flush().is_err());
        assert!(s.flush().is_ok());
        assert!(fs.called("claim_flush"));
    }

    #[test]
    fn claims_are_visible_through_the_fake_accessors() {
        use flux_fs::{
            ClaimKey, ClaimRecord, ClaimStatus, ClaimStore, DestinationRoot, DirHandle, Durability,
            FluxPathKey, ObjectId,
        };
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/d")).unwrap();
        let d = fs.destination_root(Path::new("/d")).unwrap();
        let mut s = d.create_claim_store(OsStr::new("state.db"), Durability::Normal).unwrap();
        assert_eq!(fs.claim_count(), 0);
        let parent = ObjectId { volume: 1, index: 9 };
        let rec = ClaimRecord { target: FluxPathKey(b"t".to_vec()), status: ClaimStatus::Existing };
        s.insert_if_absent(&ClaimKey::new(parent, OsStr::new("x")), &rec).unwrap();
        assert_eq!(fs.claim_count(), 1);
        assert_eq!(fs.claim(parent, "x"), Some(rec.clone()));
        assert_eq!(fs.claim(parent, "y"), None);
        s.upgrade_own_claim(&ClaimKey::new(parent, OsStr::new("x")), &rec.target).unwrap();
        assert_eq!(fs.claim(parent, "x").unwrap().status, ClaimStatus::Created);
    }
}
