// Copyright (c) 2026 Costas Kirgoussios
//
// This software is licensed under the PolyForm Noncommercial License 1.0.0.
// You may obtain a copy of the License at:
// https://polyformproject.org/licenses/noncommercial/1.0.0/

//! The scratch space a bounded listing spills into: one anonymous file in a directory, carved into regions that
//! are truncated in stack order.

use crate::store::Store;
use flux_fs::{Code, DestinationRoot, DirHandle, FsError};
use std::cell::{Cell, Ref, RefCell, RefMut};
use std::collections::HashMap;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::rc::Rc;

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
    /// A destination index with at most this many names stays in memory.
    pub index_inline: usize,
    /// Candidates verified per lookup in the inode family; more are treated as none.
    pub max_candidates: usize,
    /// Blocks of the index block cache.
    pub index_cache_blocks: usize,
    /// Bytes of cache for the embedded store.
    pub index_store_cache: usize,
    /// Store rows written or removed per commit.
    pub publication_commit: usize,
    /// Bytes per index block (a power of two, at least 64).
    pub index_block: usize,
}

impl Default for Thresholds {
    fn default() -> Self {
        Thresholds {
            inline_entries: 8_192,
            run_entries: 262_144,
            merge_fan_in: 384,
            merge_buffer: 16_384,
            tick_entries: 65_536,
            index_inline: 8_192,
            max_candidates: 16,
            index_cache_blocks: 256,
            index_store_cache: 16 * 1024 * 1024,
            publication_commit: 1_024,
            index_block: 4_096,
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
        Ok(Thresholds {
            inline_entries,
            run_entries,
            merge_fan_in,
            merge_buffer,
            tick_entries,
            ..Thresholds::default()
        })
    }

    /// Replaces the destination index sizes; refuses a combination the index cannot work with.
    pub fn with_index(
        self,
        index_inline: usize,
        max_candidates: usize,
        index_cache_blocks: usize,
        index_store_cache: usize,
        publication_commit: usize,
        index_block: usize,
    ) -> Result<Thresholds, &'static str> {
        if index_inline == 0 {
            return Err("index_inline must be at least 1");
        }
        if max_candidates == 0 {
            return Err("max_candidates must be at least 1");
        }
        if index_cache_blocks < 4 {
            return Err("index_cache_blocks must be at least 4");
        }
        if publication_commit == 0 {
            return Err("publication_commit must be at least 1");
        }
        if index_block < 64 {
            return Err("index_block must be at least 64");
        }
        if !index_block.is_power_of_two() {
            return Err("index_block must be a power of two");
        }
        Ok(Thresholds {
            index_inline,
            max_candidates,
            index_cache_blocks,
            index_store_cache,
            publication_commit,
            index_block,
            ..self
        })
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

/// The owner every index lane's regions are taken with: index frames nest in stack order and no two walks share a
/// lane, so the one-active-walk check does not apply to them.
pub(crate) const INDEX_OWNER: u64 = u64::MAX;

/// One anonymous file of the space, with its own logical end.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Lane {
    Walk,
    Names,
    Inodes,
    Identity,
}

impl Lane {
    const COUNT: usize = 4;

    pub(crate) fn idx(self) -> usize {
        match self {
            Lane::Walk => 0,
            Lane::Names => 1,
            Lane::Inodes => 2,
            Lane::Identity => 3,
        }
    }
}

/// The per-file state of one lane.
struct LaneState {
    backing: RefCell<Backing>,
    /// The logical end of the file.
    end: Cell<u64>,
    live_regions: Cell<usize>,
    holder: Cell<u64>,
}

impl LaneState {
    fn new() -> LaneState {
        LaneState {
            backing: RefCell::new(Backing::NotCreated),
            end: Cell::new(0),
            live_regions: Cell::new(0),
            holder: Cell::new(0),
        }
    }
}

/// The first cause that made a space unusable. `FsError` is not `Clone` and its source must never be rebuilt with
/// context, so the code, the kind and the text are kept and the error is re-made from them.
pub(crate) struct Poison {
    cause: RefCell<Option<(Code, std::io::ErrorKind, String)>>,
}

impl Poison {
    pub(crate) fn new() -> Poison {
        Poison { cause: RefCell::new(None) }
    }

    /// Keeps the FIRST cause; a later one is ignored.
    pub(crate) fn set(&self, cause: FsError) {
        let mut slot = self.cause.borrow_mut();
        if slot.is_none() {
            *slot = Some((cause.code, cause.source.kind(), cause.source.to_string()));
        }
    }

    pub(crate) fn error(&self) -> Option<FsError> {
        self.cause.borrow().as_ref().map(|(code, kind, message)| {
            FsError::new(
                *code,
                std::io::Error::new(*kind, format!("scratch space is unusable: {message}")),
            )
        })
    }

    pub(crate) fn check(&self) -> Result<(), FsError> {
        match self.error() {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
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
    lanes: [LaneState; Lane::COUNT],
    poison: Rc<Poison>,
    store: RefCell<Option<Store>>,
    owners: Cell<u64>,
    ordinals: Cell<u64>,
    cache: RefCell<BlockCache>,
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
            lanes: std::array::from_fn(|_| LaneState::new()),
            poison: Rc::new(Poison::new()),
            store: RefCell::new(None),
            owners: Cell::new(0),
            ordinals: Cell::new(0),
            cache: RefCell::new(BlockCache::new(thresholds.index_cache_blocks)),
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

    /// The tier 2 key of a directory with no Strong identity: 1, 2, 3, ...
    pub(crate) fn next_ordinal(&self) -> u64 {
        let n = self.ordinals.get() + 1;
        self.ordinals.set(n);
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

    fn lane(&self, lane: Lane) -> &LaneState {
        &self.lanes[lane.idx()]
    }

    /// A region of `lane` starting at the lane's current length. Creates the lane's file on its first call. Only
    /// `Lane::Walk` has the one-active-walk check; the index lanes take `INDEX_OWNER` and nest freely.
    pub(crate) fn begin(&self, lane: Lane, owner: u64) -> Result<Region<'_, 'h>, FsError> {
        self.check_usable()?;
        let state = self.lane(lane);
        if lane == Lane::Walk && state.live_regions.get() > 0 && state.holder.get() != owner {
            return Err(fatal("one scratch space is shared by two walks"));
        }
        self.ensure_open(lane)?;
        state.holder.set(owner);
        state.live_regions.set(state.live_regions.get() + 1);
        Ok(Region { space: self, lane, start: state.end.get() })
    }

    pub(crate) fn write_at(&self, lane: Lane, offset: u64, bytes: &[u8]) -> Result<(), FsError> {
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
        let state = self.lane(lane);
        let backing = state.backing.borrow();
        let Backing::Open(file) = &*backing else {
            return Err(self.not_open(&backing));
        };
        write_all_at(file, bytes, offset).map_err(|e| self.io_error(e))?;
        let new_end = offset + bytes.len() as u64;
        if new_end > state.end.get() {
            state.end.set(new_end);
        }
        Ok(())
    }

    pub(crate) fn read_at(
        &self,
        lane: Lane,
        offset: u64,
        buf: &mut [u8],
    ) -> Result<usize, FsError> {
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
        let backing = self.lane(lane).backing.borrow();
        let Backing::Open(file) = &*backing else {
            return Err(self.not_open(&backing));
        };
        read_fully_at(file, buf, offset).map_err(|e| self.io_error(e))
    }

    /// The logical end of the lane's file.
    pub(crate) fn len(&self, lane: Lane) -> u64 {
        self.lane(lane).end.get()
    }

    /// `len` bytes of `lane` at `offset`, through the block cache. A `len` above `index_block` (an oversized frame) is
    /// read uncached. Short files are an error: an index block never ends early.
    pub(crate) fn read_block(
        &self,
        lane: Lane,
        offset: u64,
        len: usize,
    ) -> Result<Rc<[u8]>, FsError> {
        self.check_usable()?;
        let cached = len <= self.thresholds.index_block;
        if cached && let Some(hit) = self.cache.borrow_mut().get(lane, offset) {
            return Ok(hit);
        }
        let mut buf = vec![0u8; len];
        let got = self.read_at(lane, offset, &mut buf)?;
        if got < len {
            return Err(fatal("an index block ends before its recorded length"));
        }
        let data: Rc<[u8]> = Rc::from(buf);
        let mut cache = self.cache.borrow_mut();
        cache.misses += 1;
        if cached {
            cache.insert(lane, offset, Rc::clone(&data));
        }
        Ok(data)
    }

    /// Drops every cached block of `lane` that reaches `start` or beyond. Called by `truncate`, so a truncated region
    /// is never served to the family built at the same offsets next.
    pub(crate) fn invalidate_from(&self, lane: Lane, start: u64) {
        self.cache.borrow_mut().invalidate_from(lane, start);
    }

    /// Blocks read from the file (cache misses and uncached reads) since the space was made.
    // Read only by tests so far; no non-test caller reports it yet.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn cache_misses(&self) -> u64 {
        self.cache.borrow().misses
    }

    #[cfg(test)]
    pub(crate) fn reset_cache_misses(&self) {
        self.cache.borrow_mut().misses = 0;
    }

    /// A fresh anonymous file from the home, for a family's cascade passes: not a lane and not tracked; the caller
    /// drops it. A deferred home is resolved first, exactly as `ensure_open` does.
    pub(crate) fn transient(&self) -> Result<File, FsError> {
        self.check_usable()?;
        self.resolve_home();
        match &*self.home.borrow() {
            Home::Deferred(_) | Home::Unplaced => {
                Err(fatal("scratch space used before it was placed"))
            }
            Home::Failed { code, message } => {
                Err(FsError::new(*code, std::io::Error::other(message.clone())))
            }
            Home::Placed { create, shown } => create().map_err(|e| {
                FsError::new(
                    e.code,
                    std::io::Error::new(
                        e.source.kind(),
                        format!("{}: {}", shown.display(), e.source),
                    ),
                )
            }),
        }
    }

    /// The tier-2 store, created on first use over an unlinked file of its own. Any error is poisoned and returned;
    /// on a poisoned space the poison error comes back and nothing is touched.
    pub(crate) fn store(&self) -> Result<RefMut<'_, Store>, FsError> {
        self.check_usable()?;
        let mut slot = self
            .store
            .try_borrow_mut()
            .map_err(|_| fatal("the scratch store is already borrowed"))?;
        if slot.is_none() {
            let created = self.transient().and_then(|file| {
                Store::create(
                    file,
                    self.shown(),
                    self.thresholds.index_store_cache,
                    self.thresholds.publication_commit,
                    self.poison_handle(),
                )
            });
            match created {
                Ok(store) => *slot = Some(store),
                Err(e) => {
                    self.poison(FsError::new(
                        e.code,
                        std::io::Error::new(e.source.kind(), e.source.to_string()),
                    ));
                    return Err(e);
                }
            }
            #[cfg(test)]
            crate::store::faults::created();
        }
        RefMut::filter_map(slot, Option::as_mut).map_err(|_| fatal("the scratch store is missing"))
    }

    /// Makes the store's final commit, if a store exists: a run that is otherwise ending successfully calls it
    /// before `close()`. Its error is the fatal scratch error.
    pub(crate) fn finish_store(&self) -> Result<(), FsError> {
        self.check_usable()?;
        match self.store.try_borrow_mut() {
            Ok(mut slot) => match slot.as_mut() {
                Some(store) => store.finish(),
                None => Ok(()),
            },
            Err(_) => Err(fatal("the scratch store is already borrowed")),
        }
    }

    /// The lane's file, created if needed (the same path as `begin`), for a worker thread that writes it through
    /// `&File` with `write_all_at`.
    ///
    /// INVARIANT: while the returned `Ref` is held no call may take `borrow_mut` on that lane's backing (`ensure_open`
    /// does). The tick the listing thread calls while waiting is the beat-and-guard closure, which touches the lock
    /// file, never the scratch space.
    pub(crate) fn lane_file(&self, lane: Lane) -> Result<Ref<'_, File>, FsError> {
        self.check_usable()?;
        self.ensure_open(lane)?;
        Ref::filter_map(self.lane(lane).backing.borrow(), |b| match b {
            Backing::Open(file) => Some(file),
            _ => None,
        })
        .map_err(|b| self.not_open(&b))
    }

    /// Raises the lane's logical end to `end` (never lowers it): the listing thread calls it after joining the
    /// workers that wrote the lane through `lane_file`.
    pub(crate) fn set_end(&self, lane: Lane, end: u64) {
        let state = self.lane(lane);
        if end > state.end.get() {
            state.end.set(end);
        }
    }

    /// Makes the space unusable, keeping the FIRST cause.
    pub(crate) fn poison(&self, cause: FsError) {
        self.poison.set(cause);
    }

    pub(crate) fn poison_error(&self) -> Option<FsError> {
        self.poison.error()
    }

    /// The poison, shared with the store that lives inside the space.
    pub(crate) fn poison_handle(&self) -> Rc<Poison> {
        Rc::clone(&self.poison)
    }

    // Read only by tests so far: every scratch call checks the flag itself.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn poisoned(&self) -> bool {
        self.poison_error().is_some()
    }

    /// Drops the file (on Windows the delete-on-close handle, without which the workspace cannot be removed). A
    /// later `begin` is a fatal error.
    pub fn close(&self) {
        // The store holds a file of its own: it goes first.
        *self.store.borrow_mut() = None;
        for state in &self.lanes {
            *state.backing.borrow_mut() = Backing::Closed;
        }
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
    pub(crate) fn physical_len(&self, lane: Lane) -> u64 {
        match &*self.lane(lane).backing.borrow() {
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
        self.poison.check()
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

    fn ensure_open(&self, lane: Lane) -> Result<(), FsError> {
        let mut backing = self.lane(lane).backing.borrow_mut();
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

    /// Cuts the lane's file back to `start`. A failure poisons the space.
    pub(crate) fn truncate(&self, lane: Lane, start: u64) {
        self.invalidate_from(lane, start);
        #[cfg(test)]
        if self.truncate_fails.replace(false) {
            self.poison(fatal("a truncate failed"));
            return;
        }
        if let Backing::Open(file) = &*self.lane(lane).backing.borrow()
            && let Err(e) = file.set_len(start)
        {
            self.poison(FsError::new(
                FsError::classify(&e),
                std::io::Error::new(e.kind(), format!("a truncate failed: {e}")),
            ));
        }
    }
}

/// One cached block.
struct Slot {
    lane: Lane,
    offset: u64,
    data: Rc<[u8]>,
    /// Set by a hit, cleared by the clock hand passing over.
    referenced: bool,
}

/// A fixed number of index blocks with clock (second chance) eviction, keyed by lane and offset.
pub(crate) struct BlockCache {
    slots: Vec<Option<Slot>>,
    map: HashMap<(Lane, u64), usize>,
    hand: usize,
    capacity: usize,
    misses: u64,
}

impl BlockCache {
    fn new(capacity: usize) -> BlockCache {
        BlockCache { slots: Vec::new(), map: HashMap::new(), hand: 0, capacity, misses: 0 }
    }

    fn get(&mut self, lane: Lane, offset: u64) -> Option<Rc<[u8]>> {
        let i = *self.map.get(&(lane, offset))?;
        let slot = self.slots[i].as_mut()?;
        slot.referenced = true;
        Some(Rc::clone(&slot.data))
    }

    fn insert(&mut self, lane: Lane, offset: u64, data: Rc<[u8]>) {
        let slot = Slot { lane, offset, data, referenced: false };
        let index = if self.slots.len() < self.capacity {
            self.slots.push(None);
            self.slots.len() - 1
        } else {
            loop {
                let i = self.hand;
                self.hand = (self.hand + 1) % self.slots.len();
                match &mut self.slots[i] {
                    None => break i,
                    Some(s) if s.referenced => s.referenced = false,
                    Some(s) => {
                        self.map.remove(&(s.lane, s.offset));
                        break i;
                    }
                }
            }
        };
        self.map.insert((lane, offset), index);
        self.slots[index] = Some(slot);
    }

    fn invalidate_from(&mut self, lane: Lane, start: u64) {
        for cell in &mut self.slots {
            if let Some(s) = cell
                && s.lane == lane
                && s.offset + s.data.len() as u64 > start
            {
                self.map.remove(&(s.lane, s.offset));
                *cell = None;
            }
        }
    }
}

/// A span of the file from `start` to its end at the time it is dropped; dropping truncates the file back to
/// `start`, unless `start` is above the file's current end (a parent region already dropped and cut the file below
/// it): then the file is left alone, since a truncate would pad it with zeros. Regions nest like a stack.
pub(crate) struct Region<'s, 'h> {
    space: &'s ScratchSpace<'h>,
    lane: Lane,
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
        let state = self.space.lane(self.lane);
        if self.start <= state.end.get() {
            self.space.truncate(self.lane, self.start);
            state.end.set(self.start);
        }
        state.live_regions.set(state.live_regions.get().saturating_sub(1));
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

/// Opens the operator's `--scratch-dir`. A failure names the path: the code is taken from the error first, then the
/// message is rebuilt to carry the location.
pub(crate) fn open_explicit<F: DestinationRoot>(fs: &F, path: &Path) -> Result<F::Dir, FsError> {
    fs.destination_root(path).map_err(|e| {
        // `.source` is deliberately rebuilt to name the path; consumers must use `.code`, not `raw_os_error`.
        FsError::new(
            e.code,
            std::io::Error::new(e.source.kind(), format!("{}: {}", path.display(), e.source)),
        )
    })
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
        return Ok((open_explicit(fs, path)?, path.to_path_buf()));
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
pub(crate) fn write_all_at(file: &File, bytes: &[u8], offset: u64) -> std::io::Result<()> {
    std::os::unix::fs::FileExt::write_all_at(file, bytes, offset)
}

#[cfg(windows)]
pub(crate) fn write_all_at(file: &File, mut bytes: &[u8], mut offset: u64) -> std::io::Result<()> {
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
pub(crate) fn read_fully_at(file: &File, buf: &mut [u8], offset: u64) -> std::io::Result<usize> {
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
        space.physical_len(Lane::Walk)
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
    fn lanes_have_independent_ends_and_regions() {
        let fs = FaultFs::new();
        let space = placed(&fs);
        let walk = space.begin(Lane::Walk, 1).ok().unwrap();
        space.write_at(Lane::Walk, walk.start(), &[1u8; 10]).unwrap();
        let names = space.begin(Lane::Names, INDEX_OWNER).ok().unwrap();
        space.write_at(Lane::Names, names.start(), &[2u8; 5]).unwrap();
        assert_eq!((space.len(Lane::Walk), space.len(Lane::Names)), (10, 5));
        assert_eq!(space.physical_len(Lane::Names), 5);
        drop(names);
        assert_eq!((space.len(Lane::Walk), space.len(Lane::Names)), (10, 0));
        assert_eq!(space.physical_len(Lane::Walk), 10);
        let mut buf = [0u8; 10];
        assert_eq!(space.read_at(Lane::Walk, 0, &mut buf).unwrap(), 10);
        assert_eq!(buf, [1u8; 10]);
        drop(walk);
        assert_eq!(space.len(Lane::Walk), 0);
    }

    #[test]
    fn each_lane_is_created_on_its_first_region() {
        let fs = FaultFs::new();
        let space = placed(&fs);
        let _w = space.begin(Lane::Walk, 1).ok().unwrap();
        assert_eq!(creations(&fs), 1, "{:?}", fs.calls());
        let _n = space.begin(Lane::Names, INDEX_OWNER).ok().unwrap();
        let _n2 = space.begin(Lane::Names, INDEX_OWNER).ok().unwrap();
        assert_eq!(creations(&fs), 2, "{:?}", fs.calls());
    }

    #[test]
    fn a_transient_file_is_not_a_lane() {
        let fs = FaultFs::new();
        let space = placed(&fs);
        let _w = space.begin(Lane::Walk, 1).ok().unwrap();
        let _n = space.begin(Lane::Names, INDEX_OWNER).ok().unwrap();
        assert_eq!(creations(&fs), 2);
        let file = space.transient().unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(creations(&fs), 3, "{:?}", fs.calls());
        for lane in [Lane::Walk, Lane::Names, Lane::Inodes, Lane::Identity] {
            assert_eq!(space.len(lane), 0);
        }
        drop(file);
        assert_eq!(creations(&fs), 3);
    }

    #[test]
    fn a_transient_file_places_a_deferred_home() {
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/s")).unwrap();
        let asked = std::rc::Rc::new(Cell::new(0));
        let space = ScratchSpace::unplaced(Thresholds::default(), None);
        let counter = std::rc::Rc::clone(&asked);
        let fs_ref = &fs;
        space.place_deferred(move || {
            counter.set(counter.get() + 1);
            Ok((fs_ref.destination_root(Path::new("/s"))?, PathBuf::from("/s")))
        });
        assert_eq!(asked.get(), 0);
        assert!(space.transient().is_ok());
        assert_eq!(asked.get(), 1);
        assert!(space.transient().is_ok());
        assert_eq!(asked.get(), 1, "the producer ran once");
    }

    #[test]
    fn index_lanes_nest_regions_from_the_index_owner() {
        let fs = FaultFs::new();
        let space = placed(&fs);
        let a = space.begin(Lane::Names, INDEX_OWNER);
        let b = space.begin(Lane::Names, INDEX_OWNER);
        assert!(a.is_ok() && b.is_ok());
        let first = space.begin(Lane::Walk, 1).ok().unwrap();
        let err = space.begin(Lane::Walk, 2).err().expect("refused");
        assert!(err.to_string().contains("shared by two walks"), "{err}");
        drop(first);
    }

    #[test]
    fn lane_file_creates_the_lanes_file_and_a_closed_space_refuses_it() {
        let fs = FaultFs::new();
        let space = placed(&fs);
        assert_eq!(creations(&fs), 0);
        drop(space.lane_file(Lane::Inodes).unwrap_or_else(|e| panic!("{e}")));
        assert_eq!(creations(&fs), 1, "{:?}", fs.calls());
        drop(space.lane_file(Lane::Inodes).unwrap_or_else(|e| panic!("{e}")));
        assert_eq!(creations(&fs), 1, "the second call reuses the file");
        space.close();
        let err = space.lane_file(Lane::Inodes).expect_err("refused");
        assert!(err.to_string().contains("closed"), "{err}");
        let err = space.lane_file(Lane::Names).expect_err("refused");
        assert!(err.to_string().contains("closed"), "{err}");
    }

    #[test]
    fn set_end_only_raises_the_lanes_end() {
        let fs = FaultFs::new();
        let space = placed(&fs);
        space.set_end(Lane::Names, 10);
        assert_eq!(space.len(Lane::Names), 10);
        space.set_end(Lane::Names, 4);
        assert_eq!(space.len(Lane::Names), 10, "never lowered");
        space.set_end(Lane::Names, 12);
        assert_eq!((space.len(Lane::Names), space.len(Lane::Walk)), (12, 0));
    }

    #[test]
    fn a_closed_space_refuses_a_region_on_an_index_lane() {
        let fs = FaultFs::new();
        let space = placed(&fs);
        drop(space.begin(Lane::Names, INDEX_OWNER).ok().unwrap());
        space.close();
        for lane in [Lane::Names, Lane::Inodes, Lane::Identity] {
            let err = space.begin(lane, INDEX_OWNER).err().expect("refused");
            assert!(err.to_string().contains("closed"), "{err}");
        }
    }

    #[test]
    fn a_poisoned_space_reports_its_first_cause() {
        let fs = FaultFs::new();
        let space = placed(&fs);
        space.poison(FsError::new(Code::DiskFull, std::io::Error::other("a")));
        space.poison(FsError::new(Code::IoError, std::io::Error::other("b")));
        assert_eq!(space.poison_error().unwrap().code, Code::DiskFull);
        assert!(space.poisoned());
        for lane in [Lane::Walk, Lane::Names, Lane::Inodes, Lane::Identity] {
            let err = space.begin(lane, INDEX_OWNER).err().expect("refused");
            assert_eq!(err.code, Code::DiskFull);
            let text = err.to_string();
            assert!(text.contains("scratch space is unusable: a"), "{text}");
            assert!(!text.contains(": b"), "{text}");
        }
    }

    #[test]
    fn ordinals_start_at_one_and_count_up() {
        let space = ScratchSpace::unplaced(Thresholds::default(), None);
        assert_eq!((space.next_ordinal(), space.next_ordinal(), space.next_ordinal()), (1, 2, 3));
    }

    #[test]
    fn with_index_refuses_a_zero_threshold_and_a_small_block() {
        let d = Thresholds::default();
        assert!(d.with_index(0, 16, 256, 1, 1024, 4096).is_err());
        assert!(d.with_index(4, 0, 256, 1, 1024, 4096).is_err());
        assert!(d.with_index(4, 2, 3, 1, 1024, 4096).is_err());
        assert!(d.with_index(4, 2, 4, 1, 0, 4096).is_err());
        assert!(d.with_index(4, 2, 4, 1, 2, 32).is_err());
        assert!(d.with_index(4, 2, 4, 1, 2, 100).is_err());
        let t = d.with_index(4, 2, 4, 1, 2, 64).unwrap();
        assert_eq!((t.index_inline, t.max_candidates, t.index_cache_blocks), (4, 2, 4));
        assert_eq!((t.index_store_cache, t.publication_commit, t.index_block), (1, 2, 64));
        let t = Thresholds::new(4, 8, 2, 16, 3).unwrap();
        assert_eq!((t.index_inline, t.index_block), (8_192, 4_096));
        assert_eq!(
            (t.max_candidates, t.index_cache_blocks, t.index_store_cache, t.publication_commit),
            (16, 256, 16 * 1024 * 1024, 1_024)
        );
    }

    #[test]
    fn nothing_is_created_until_the_first_region() {
        let fs = FaultFs::new();
        let space = placed(&fs);
        assert_eq!(creations(&fs), 0, "{:?}", fs.calls());
        let _r = space.begin(Lane::Walk, 1).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(creations(&fs), 1, "{:?}", fs.calls());
    }

    #[test]
    fn an_unplaced_space_refuses_to_spill() {
        let space = ScratchSpace::unplaced(Thresholds::default(), None);
        let err = space.begin(Lane::Walk, 1).err().expect("refused");
        assert!(err.to_string().contains("before it was placed"), "{err}");
    }

    #[test]
    fn nested_regions_truncate_in_lifo_order() {
        let fs = FaultFs::new();
        let space = placed(&fs);
        let a = space.begin(Lane::Walk, 1).ok().unwrap();
        space.write_at(Lane::Walk, a.start(), &[1u8; 10]).unwrap();
        {
            let b = space.begin(Lane::Walk, 1).ok().unwrap();
            assert_eq!(b.start(), 10);
            space.write_at(Lane::Walk, b.start(), &[2u8; 5]).unwrap();
            assert_eq!(space.len(Lane::Walk), 15);
            assert_eq!(physical_len(&space), 15);
        }
        assert_eq!(space.len(Lane::Walk), 10);
        assert_eq!(physical_len(&space), 10);
        let mut buf = [0u8; 10];
        assert_eq!(space.read_at(Lane::Walk, 0, &mut buf).unwrap(), 10);
        assert_eq!(buf, [1u8; 10]);
        drop(a);
        assert_eq!(space.len(Lane::Walk), 0);
        assert_eq!(physical_len(&space), 0);
    }

    #[test]
    fn a_parent_dropped_before_its_child_leaves_the_file_empty() {
        let fs = FaultFs::new();
        let space = placed(&fs);
        let a = space.begin(Lane::Walk, 1).ok().unwrap();
        space.write_at(Lane::Walk, a.start(), &[1u8; 10]).unwrap();
        let b = space.begin(Lane::Walk, 1).ok().unwrap();
        space.write_at(Lane::Walk, b.start(), &[2u8; 5]).unwrap();
        drop(a);
        assert_eq!((space.len(Lane::Walk), physical_len(&space)), (0, 0));
        drop(b);
        assert_eq!((space.len(Lane::Walk), physical_len(&space)), (0, 0));
        assert!(space.begin(Lane::Walk, 1).is_ok(), "the space is still usable");
    }

    #[test]
    fn an_injected_write_failure_is_returned_and_the_file_ends_empty() {
        let fs = FaultFs::new();
        let space = placed(&fs);
        space.fail_writes_after(1);
        {
            let r = space.begin(Lane::Walk, 1).ok().unwrap();
            space.write_at(Lane::Walk, r.start(), b"abc").unwrap();
            let err = space.write_at(Lane::Walk, r.start() + 3, b"def").unwrap_err();
            assert_eq!(err.code, Code::DiskFull);
            // A partial write that failed halfway leaves bytes physically past the logical end.
            if let Backing::Open(file) = &*space.lanes[Lane::Walk.idx()].backing.borrow() {
                write_all_at(file, b"de", r.start() + 3).unwrap();
            }
            assert_eq!((space.len(Lane::Walk), physical_len(&space)), (3, 5));
        }
        assert_eq!(space.len(Lane::Walk), 0);
        assert_eq!(physical_len(&space), 0);
    }

    #[test]
    fn a_second_walk_cannot_begin_while_the_first_holds_a_region() {
        let fs = FaultFs::new();
        let space = placed(&fs);
        let first = space.begin(Lane::Walk, 1).ok().unwrap();
        let err = space.begin(Lane::Walk, 2).err().expect("refused");
        assert!(err.to_string().contains("shared by two walks"), "{err}");
        drop(first);
        assert!(space.begin(Lane::Walk, 2).is_ok());
    }

    #[test]
    fn a_closed_space_refuses_to_spill() {
        let fs = FaultFs::new();
        let space = placed(&fs);
        drop(space.begin(Lane::Walk, 1).ok().unwrap());
        space.close();
        assert!(space.begin(Lane::Walk, 1).is_err());
    }

    #[test]
    fn a_failed_creation_is_returned_on_every_attempt() {
        let fs = FaultFs::new();
        let space = placed(&fs);
        fs.fail("create_scratch", Code::PermissionDenied);
        for _ in 0..2 {
            let err = space.begin(Lane::Walk, 1).err().expect("refused");
            assert_eq!(err.code, Code::PermissionDenied);
        }
        assert_eq!(creations(&fs), 1, "{:?}", fs.calls());
    }

    #[test]
    fn a_failed_truncate_poisons_every_later_call() {
        let fs = FaultFs::new();
        let space = placed(&fs);
        {
            let r = space.begin(Lane::Walk, 1).ok().unwrap();
            space.write_at(Lane::Walk, r.start(), b"abc").unwrap();
            space.fail_truncate();
        }
        assert!(space.poisoned());
        let want = "scratch space is unusable: a truncate failed";
        assert!(space.begin(Lane::Walk, 1).err().unwrap().to_string().contains(want));
        assert!(space.write_at(Lane::Walk, 0, b"x").unwrap_err().to_string().contains(want));
        assert!(
            space.read_at(Lane::Walk, 0, &mut [0u8; 1]).unwrap_err().to_string().contains(want)
        );
    }

    #[test]
    fn a_place_failure_is_remembered_until_a_spill_needs_it() {
        let space = ScratchSpace::unplaced(Thresholds::default(), None);
        let home: Result<(crate::fault_fs::FakeDirHandle, PathBuf), FsError> =
            Err(FsError::new(Code::PermissionDenied, std::io::Error::other("no temp dir")));
        space.place_deferred(move || home);
        let err = space.begin(Lane::Walk, 1).err().expect("refused");
        assert_eq!(err.code, Code::PermissionDenied);
        // Remembered, not retried: every later attempt answers the same.
        assert_eq!(
            space.begin(Lane::Walk, 1).err().expect("refused again").code,
            Code::PermissionDenied
        );
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
        let Ok(first) = space.begin(Lane::Walk, 1) else { panic!("the home resolves") };
        assert_eq!(asked.get(), 1);
        drop(first);
        drop(space.begin(Lane::Walk, 1).ok().unwrap());
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
        space.begin(Lane::Walk, 1).map(drop).unwrap_or_else(|e| panic!("opens: {e}"));
        assert!(!released.get(), "the home is held while the space is open");
        space.close();
        assert!(released.get(), "close drops what holds the workspace handle");
        let err = space.begin(Lane::Walk, 1).err().expect("refused");
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
        assert!(space.begin(Lane::Walk, 1).err().expect("refused").to_string().contains("closed"));
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
    /// Where the scratch file goes for a home chosen by the search: on Windows into its `flux-scratch` subdirectory.
    fn chosen(home: &str) -> PathBuf {
        if cfg!(windows) { Path::new(home).join("flux-scratch") } else { PathBuf::from(home) }
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
        assert_eq!(shown, chosen("/tmp"));
        fs.set_memory_backed("/tmp", Some(true));
        let (_, shown) = temporary_home_from(&fs, None, Path::new("/tmp"), &caches).unwrap();
        assert_eq!(shown, chosen("/cache"));
        // An unknown answer is not "memory-backed".
        fs.set_memory_backed("/tmp", None);
        let (_, shown) = temporary_home_from(&fs, None, Path::new("/tmp"), &caches).unwrap();
        assert_eq!(shown, chosen("/tmp"));
    }

    #[test]
    fn a_missing_explicit_scratch_dir_is_named_and_keeps_its_code() {
        use flux_fs::DestinationRoot;
        let fs = home_fs();
        let Err(bare) = fs.destination_root(Path::new("/nope")) else { panic!("absent") };
        let before = bare.code;
        let Err(e) = temporary_home_from(&fs, Some(Path::new("/nope")), Path::new("/tmp"), &[])
        else {
            panic!("the explicit directory is absent");
        };
        assert_eq!(e.code, before);
        assert!(e.source.to_string().contains("/nope"), "{}", e.source);
    }

    #[test]
    fn temporary_home_makes_an_absent_cache_and_names_the_flag_when_nothing_fits() {
        let fs = home_fs();
        fs.set_memory_backed("/tmp", Some(true));
        let caches = [PathBuf::from("/cache/sub")];
        let (_, shown) = temporary_home_from(&fs, None, Path::new("/tmp"), &caches).unwrap();
        assert_eq!(shown, chosen("/cache/sub"));
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
