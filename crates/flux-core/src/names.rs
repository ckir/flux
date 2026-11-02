// Copyright (c) 2026 Costas Kirgoussios
//
// This software is licensed under the PolyForm Noncommercial License 1.0.0.
// You may obtain a copy of the License at:
// https://polyformproject.org/licenses/noncommercial/1.0.0/

//! Destination name resolution (cut 8b): the STORED spelling of an existing entry, by listing and identity.
//!
//! Design authority: `docs/superpowers/specs/2026-10-07-cut-8b-replacement-design.md`, "Name resolution"; the hybrid
//! form (a directory above `index_inline` names) is `docs/superpowers/specs/2026-10-30-bounded-listing-design.md`
//! section 6.3.

use crate::index::{Family, FamilyBuilder, build_one, build_two};
use crate::scratch::{Lane, ScratchSpace};
use crate::spill::Keyed;
use crate::store::DirKey;
use crate::walk::Tick;
use flux_fs::{DirEntry, DirHandle, FileIdentity, FluxPathKey, FsError, Metadata, ObjectId};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ffi::{OsStr, OsString};
use std::ops::ControlFlow;

/// What an index is built over: the scratch space it may spill into and the tick that keeps the run's heartbeat alive
/// while it does. The tick is borrowed for the build only.
pub(crate) struct IndexSite<'s, 'h, 't> {
    pub scratch: &'s ScratchSpace<'h>,
    pub tick: &'t mut Tick<'t>,
}

/// Why `for_dir` produced no index.
#[derive(Debug)]
pub(crate) enum BuildError {
    /// The destination directory could not be listed.
    Listing(FsError),
    /// The scratch space (or the run's tick) failed: the run stops.
    Fatal(FsError),
}

impl BuildError {
    pub(crate) fn into_fs(self) -> FsError {
        match self {
            BuildError::Listing(e) | BuildError::Fatal(e) => e,
        }
    }
}

/// One destination directory's names: held in memory up to `index_inline` names, in sorted families and the store
/// above that.
pub(crate) struct NameIndex<'s, 'h> {
    form: Form<'s, 'h>,
    /// An index for a directory this run created is not tracked: nothing reads it (`resolve` and `owner` are reached
    /// only for a non-fresh frame), so `record_publication` records nothing.
    tracked: bool,
}

enum Form<'s, 'h> {
    Inline(Inline<'s, 'h>),
    Hybrid(Hybrid<'s, 'h>),
}

struct Inline<'s, 'h> {
    listing: BTreeSet<OsString>,
    /// Strong identity -> its spellings, in the order they were learned (listing order, then publications). `None`
    /// until the first by-identity resolution.
    table: Option<HashMap<ObjectId, Vec<OsString>>>,
    /// Spelling -> the target of this run that wrote it.
    owners: HashMap<OsString, FluxPathKey>,
    /// Publications recorded while `table` is `None`, replayed over the table when it is built.
    pending: Vec<Pending>,
    /// The `stored` arguments of the publications, in the order they were recorded: the spellings the directory
    /// held BEFORE this run replaced them, which is what a later resolution should answer with.
    learned: HashMap<OsString, u64>,
    /// The next sequence number for `learned`.
    next_seq: u64,
    /// Where the index spills when publications push it over `index_inline`; `None` never migrates.
    site: Option<&'s ScratchSpace<'h>>,
    /// The directory's trusted identity, if any: the tier 2 key after a migration.
    key: Option<ObjectId>,
    /// Every `replaced` identity ever recorded, in order. `apply` removes it from the table and `build_table` drains
    /// `pending`, so nothing else remembers a wipe once the table exists.
    wiped: Vec<ObjectId>,
}

struct Hybrid<'s, 'h> {
    space: &'s ScratchSpace<'h>,
    key: DirKey,
    names: Option<Family<'s, 'h>>,
    inodes: Option<Family<'s, 'h>>,
    identity: Option<Family<'s, 'h>>,
    /// Whether the identity pass ran (the family stays `None` when no name had a Strong identity).
    identity_built: bool,
    next_seq: u64,
    next_ids_seq: u64,
    /// Names the directory is known to hold, for the debug lines.
    entries: u64,
}

struct Pending {
    replaced: Option<ObjectId>,
    published: FileIdentity,
    spellings: Vec<OsString>,
}

#[derive(Debug, PartialEq)]
pub(crate) enum Resolved {
    Absent,
    Entry { stored: OsString, meta: Metadata },
}

/// The stored name cannot be determined (`DESTINATION_ERROR`, "cannot determine the stored name").
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum NameError {
    Unresolvable,
}

fn is_not_found(e: &FsError) -> bool {
    e.source.kind() == std::io::ErrorKind::NotFound
}

/// The same error again: `FsError` is not `Clone` and its source must never be rebuilt with context.
fn same_error(e: &FsError) -> FsError {
    FsError::new(e.code, std::io::Error::new(e.source.kind(), e.source.to_string()))
}

fn add_spellings(
    table: &mut HashMap<ObjectId, Vec<OsString>>,
    id: ObjectId,
    spellings: &[OsString],
) {
    let v = table.entry(id).or_default();
    for s in spellings {
        if !v.contains(s) {
            v.push(s.clone());
        }
    }
}

/// The 24 key bytes of an identity: the volume then the index, both big-endian (the tier 2 `ids` layout).
fn identity_key(id: ObjectId) -> [u8; 24] {
    let mut out = [0u8; 24];
    out[..8].copy_from_slice(&id.volume.to_be_bytes());
    out[8..].copy_from_slice(&id.index.to_be_bytes());
    out
}

/// The one selection rule, for both forms so they cannot drift: the spelling learned FIRST from a publication's
/// `stored` argument if any, else the lexicographically smallest; and with several spellings, one entry only when
/// this run wrote every one of them for the same target.
fn choose(
    spellings: &[OsString],
    learned: &dyn Fn(&OsStr) -> Result<Option<u64>, FsError>,
    owner: &dyn Fn(&OsStr) -> Result<Option<FluxPathKey>, FsError>,
) -> Result<Result<OsString, NameError>, FsError> {
    let mut first_learned: Option<(u64, &OsString)> = None;
    for s in spellings {
        if let Some(seq) = learned(s)? {
            match first_learned {
                Some(best) if best <= (seq, s) => {}
                _ => first_learned = Some((seq, s)),
            }
        }
    }
    let Some(chosen) = first_learned.map(|(_, s)| s).or_else(|| spellings.iter().min()) else {
        return Ok(Err(NameError::Unresolvable));
    };
    if spellings.len() > 1 {
        let first = owner(chosen)?;
        if first.is_none() {
            return Ok(Err(NameError::Unresolvable));
        }
        for s in spellings {
            if owner(s)? != first {
                return Ok(Err(NameError::Unresolvable));
            }
        }
    }
    Ok(Ok(chosen.clone()))
}

fn dir_key(key: Option<ObjectId>, space: &ScratchSpace<'_>) -> DirKey {
    key.map(DirKey::Strong).unwrap_or_else(|| DirKey::Ordinal(space.next_ordinal()))
}

/// The state of the one listing pass of `for_dir`.
struct Lister<'s, 'h> {
    scratch: &'s ScratchSpace<'h>,
    inline: usize,
    buf: Vec<DirEntry>,
    builders: Option<(FamilyBuilder<'s, 'h>, FamilyBuilder<'s, 'h>)>,
    count: usize,
}

impl<'s, 'h> Lister<'s, 'h> {
    fn add(&mut self, entry: DirEntry, tick: &mut Tick<'_>) -> Result<(), FsError> {
        self.count += 1;
        if self.builders.is_none() {
            if self.buf.len() < self.inline {
                self.buf.push(entry);
                return Ok(());
            }
            let mut names = FamilyBuilder::new(self.scratch, Lane::Names, 0);
            let mut inodes = FamilyBuilder::new(self.scratch, Lane::Inodes, 8);
            for buffered in self.buf.drain(..) {
                Self::push_pair(&mut names, &mut inodes, buffered, tick)?;
            }
            self.builders = Some((names, inodes));
        }
        if let Some((names, inodes)) = self.builders.as_mut() {
            Self::push_pair(names, inodes, entry, tick)?;
        }
        Ok(())
    }

    fn push_pair(
        names: &mut FamilyBuilder<'s, 'h>,
        inodes: &mut FamilyBuilder<'s, 'h>,
        entry: DirEntry,
        tick: &mut Tick<'_>,
    ) -> Result<(), FsError> {
        names.push(Keyed::plain(entry.clone()), tick)?;
        if let Some(n) = entry.ino.filter(|&n| n != 0) {
            let mut key = [0u8; 24];
            key[..8].copy_from_slice(&n.to_be_bytes());
            inodes.push(Keyed { key, entry }, tick)?;
        }
        Ok(())
    }
}

impl<'s, 'h> NameIndex<'s, 'h> {
    fn inline_form(
        listing: BTreeSet<OsString>,
        site: Option<&'s ScratchSpace<'h>>,
        key: Option<ObjectId>,
    ) -> Self {
        Self {
            form: Form::Inline(Inline {
                listing,
                table: None,
                owners: HashMap::new(),
                pending: Vec::new(),
                learned: HashMap::new(),
                next_seq: 0,
                site,
                key,
                wiped: Vec::new(),
            }),
            tracked: true,
        }
    }

    /// A directory that is NOT fresh but has no listing to read: empty listing, tracked, never listed. With a `site`
    /// it can migrate to the hybrid form when publications push it over `index_inline`.
    pub(crate) fn for_new_dir_with(
        site: Option<&'s ScratchSpace<'h>>,
        key: Option<ObjectId>,
    ) -> Self {
        Self::inline_form(BTreeSet::new(), site, key)
    }

    /// `for_new_dir_with(None, None)`: never migrates.
    pub(crate) fn for_new_dir() -> Self {
        Self::for_new_dir_with(None, None)
    }

    /// A directory this run created: empty listing, UNTRACKED (records nothing, is never read).
    pub(crate) fn for_created_dir() -> Self {
        Self { tracked: false, ..Self::for_new_dir() }
    }

    /// The index for a destination directory frame.
    ///
    /// - `fresh`: `for_created_dir()`, no read (whatever `listed` is).
    /// - not fresh, `None`: `for_new_dir_with(site, key)`, no read.
    /// - not fresh, `Some(dir)`, no site: `for_existing(dir)` (reads the listing once, into memory).
    /// - not fresh, `Some(dir)`, a site: ONE listing pass that keeps the first `index_inline` names in memory and,
    ///   past that, feeds the sorted `names` and `inodes` builders, then the hybrid form.
    pub(crate) fn for_dir<D: DirHandle>(
        fresh: bool,
        listed: Option<&D>,
        site: Option<IndexSite<'s, 'h, '_>>,
        key: Option<ObjectId>,
    ) -> Result<Self, BuildError> {
        if fresh {
            return Ok(Self::for_created_dir());
        }
        let Some(dir) = listed else {
            return Ok(Self::for_new_dir_with(site.map(|s| s.scratch), key));
        };
        let Some(IndexSite { scratch, tick }) = site else {
            return Self::for_existing(dir).map_err(BuildError::Listing);
        };
        let mut lister = Lister {
            scratch,
            inline: scratch.thresholds().index_inline,
            buf: Vec::new(),
            builders: None,
            count: 0,
        };
        let mut fatal: Option<FsError> = None;
        let listing = dir.read_dir_each(&mut |entry| match lister.add(entry, &mut *tick) {
            Ok(()) => Ok(ControlFlow::Continue(())),
            Err(e) => {
                let copy = same_error(&e);
                fatal = Some(e);
                Err(copy)
            }
        });
        if let Some(e) = fatal {
            return Err(BuildError::Fatal(e));
        }
        listing.map_err(BuildError::Listing)?;
        let entries = lister.count;
        let Some((names, inodes)) = lister.builders else {
            let listing = lister.buf.into_iter().map(|e| e.name).collect();
            return Ok(Self::inline_form(listing, Some(scratch), key));
        };
        let names = names.into_pending().map_err(BuildError::Fatal)?;
        let inodes = inodes.into_pending().map_err(BuildError::Fatal)?;
        let (names, inodes) =
            build_two(scratch, names, inodes, &mut *tick).map_err(BuildError::Fatal)?;
        tracing::debug!(entries, "destination index spilled");
        Ok(Self {
            form: Form::Hybrid(Hybrid {
                space: scratch,
                key: dir_key(key, scratch),
                names,
                inodes,
                identity: None,
                identity_built: false,
                next_seq: 0,
                next_ids_seq: 1,
                entries: entries as u64,
            }),
            tracked: true,
        })
    }

    /// Whether this index records publications (false for a directory this run created).
    #[cfg(test)]
    pub(crate) fn is_tracked(&self) -> bool {
        self.tracked
    }

    #[cfg(test)]
    pub(crate) fn is_hybrid(&self) -> bool {
        matches!(self.form, Form::Hybrid(_))
    }

    #[cfg(test)]
    pub(crate) fn identity_built(&self) -> bool {
        matches!(&self.form, Form::Hybrid(h) if h.identity_built)
    }

    #[cfg(test)]
    fn inline(&self) -> &Inline<'s, 'h> {
        match &self.form {
            Form::Inline(i) => i,
            Form::Hybrid(_) => panic!("the index is hybrid"),
        }
    }

    /// A pre-existing directory: lists it once, into memory.
    pub(crate) fn for_existing<D: DirHandle>(dir: &D) -> flux_fs::Result<Self> {
        let mut listing = BTreeSet::new();
        dir.read_dir_each(&mut |e| {
            listing.insert(e.name);
            Ok(ControlFlow::Continue(()))
        })?;
        Ok(Self::inline_form(listing, None, None))
    }

    /// Whether the scratch space this index spills into is unusable (the tree asks after a failed `resolve` or
    /// `owner`).
    pub(crate) fn is_poisoned(&self) -> bool {
        let site = match &self.form {
            Form::Inline(i) => i.site,
            Form::Hybrid(h) => Some(h.space),
        };
        site.is_some_and(|s| s.poisoned())
    }

    /// The target of this run that wrote `spelling`, if any. A hybrid index reads tier 2, which can fail.
    pub(crate) fn owner(&self, spelling: &OsStr) -> Result<Option<FluxPathKey>, FsError> {
        debug_assert!(self.tracked, "an untracked index is never read");
        match &self.form {
            Form::Inline(i) => Ok(i.owners.get(spelling).cloned()),
            Form::Hybrid(h) => h.space.store()?.owner(&h.key, spelling),
        }
    }

    /// The stored spelling of `planned`, or `Absent`.
    ///
    /// An exact listing hit whose entry vanished before the stat returns the OUTER `Err(NotFound)`, not `Absent`
    /// (intended: the listing said it exists, so a disappearance is a fault of the run, not an absent name).
    ///
    /// When several spellings of one object belong to the same target the answer is order-independent: the spelling
    /// learned FIRST from a publication's `stored` argument if any, else the lexicographically smallest.
    ///
    /// `tick` keeps the heartbeat alive while a hybrid index builds its identity family; an inline index never uses it.
    pub(crate) fn resolve<D: DirHandle>(
        &mut self,
        dir: &D,
        planned: &OsStr,
        tick: &mut Tick<'_>,
    ) -> Result<Result<Resolved, NameError>, FsError> {
        debug_assert!(self.tracked, "an untracked index is never read");
        match &mut self.form {
            Form::Inline(i) => i.resolve(dir, planned),
            Form::Hybrid(h) => h.resolve(dir, planned, tick),
        }
    }

    /// After a target published: `stored` is the pre-publication stored name (`None` for a new file), `planned` the
    /// name published, `replaced` the replaced object's identity, `published` its new identity.
    pub(crate) fn record_publication(
        &mut self,
        target: &FluxPathKey,
        stored: Option<&OsStr>,
        planned: &OsStr,
        replaced: Option<ObjectId>,
        published: FileIdentity,
    ) {
        if !self.tracked {
            return;
        }
        let migrated = match &mut self.form {
            Form::Inline(i) => {
                i.record(target, stored, planned, replaced, published);
                match i.site {
                    Some(site) if i.listing.len() > site.thresholds().index_inline => i.migrate(),
                    _ => None,
                }
            }
            Form::Hybrid(h) => {
                h.record(target, stored, planned, replaced, published);
                None
            }
        };
        if let Some(h) = migrated {
            self.form = Form::Hybrid(h);
        }
    }

    /// Ends the index: a hybrid one drops its families (their regions truncate the lanes, the cache is invalidated)
    /// and removes its rows from the store; an inline one has nothing to release.
    pub(crate) fn retire(self, tick: &mut Tick<'_>) -> Result<(), FsError> {
        match self.form {
            Form::Inline(_) => Ok(()),
            Form::Hybrid(h) => {
                let Hybrid { space, key, names, inodes, identity, .. } = h;
                drop(identity);
                drop(inodes);
                drop(names);
                space.store()?.retire(&key, tick)
            }
        }
    }
}

impl<'s, 'h> Inline<'s, 'h> {
    fn resolve<D: DirHandle>(
        &mut self,
        dir: &D,
        planned: &OsStr,
    ) -> Result<Result<Resolved, NameError>, FsError> {
        if self.listing.contains(planned) {
            let meta = dir.metadata(planned)?;
            return Ok(Ok(Resolved::Entry { stored: planned.to_os_string(), meta }));
        }
        let meta = match dir.metadata(planned) {
            Ok(meta) => meta,
            Err(e) if is_not_found(&e) => return Ok(Ok(Resolved::Absent)),
            Err(e) => return Err(e),
        };
        let FileIdentity::Strong(id) = meta.identity else {
            return Ok(Err(NameError::Unresolvable));
        };
        self.build_table(dir)?;
        let spellings = match self.table.as_ref().and_then(|t| t.get(&id)) {
            Some(s) if !s.is_empty() => s,
            _ => return Ok(Err(NameError::Unresolvable)),
        };
        let chosen = choose(spellings, &|s| Ok(self.learned.get(s).copied()), &|s| {
            Ok(self.owners.get(s).cloned())
        })?;
        Ok(chosen.map(|stored| Resolved::Entry { stored, meta }))
    }

    /// Stat every listing entry once per directory. A failed build is not remembered, so the next call retries.
    fn build_table<D: DirHandle>(&mut self, dir: &D) -> Result<(), FsError> {
        if self.table.is_some() {
            return Ok(());
        }
        let mut table: HashMap<ObjectId, Vec<OsString>> = HashMap::new();
        for name in &self.listing {
            match dir.metadata(name) {
                Ok(m) => {
                    if let FileIdentity::Strong(id) = m.identity {
                        add_spellings(&mut table, id, std::slice::from_ref(name));
                    }
                }
                // An entry deleted since the listing is no longer anyone's name: skipped, not a failure.
                Err(e) if is_not_found(&e) => {}
                // Anything else is a real fault and propagates (the table stays unbuilt).
                Err(e) => return Err(e),
            }
        }
        for p in std::mem::take(&mut self.pending) {
            Self::apply(&mut table, &p);
        }
        self.table = Some(table);
        Ok(())
    }

    fn apply(table: &mut HashMap<ObjectId, Vec<OsString>>, p: &Pending) {
        if let Some(old) = p.replaced {
            table.remove(&old);
        }
        if let FileIdentity::Strong(id) = p.published {
            add_spellings(table, id, &p.spellings);
        }
    }

    fn record(
        &mut self,
        target: &FluxPathKey,
        stored: Option<&OsStr>,
        planned: &OsStr,
        replaced: Option<ObjectId>,
        published: FileIdentity,
    ) {
        let mut spellings = Vec::with_capacity(2);
        if let Some(s) = stored {
            if !self.learned.contains_key(s) {
                self.learned.insert(s.to_os_string(), self.next_seq);
                self.next_seq += 1;
            }
            spellings.push(s.to_os_string());
        }
        if !spellings.iter().any(|s| s == planned) {
            spellings.push(planned.to_os_string());
        }
        for s in &spellings {
            self.listing.insert(s.clone());
            self.owners.insert(s.clone(), target.clone());
        }
        if self.site.is_some()
            && let Some(old) = replaced
        {
            self.wiped.push(old);
        }
        let p = Pending { replaced, published, spellings };
        match &mut self.table {
            Some(table) => Self::apply(table, &p),
            None => self.pending.push(p),
        }
    }

    /// Moves everything this index knows into tier 2 and returns the hybrid that now stands for it. A store error
    /// poisons the space (the store does it); the hybrid is returned all the same, and its next `resolve`, `owner`
    /// or `retire` returns the poison, so a half-migrated index is never consulted.
    fn migrate(&mut self) -> Option<Hybrid<'s, 'h>> {
        let space = self.site?;
        let entries = self.listing.len();
        let mut h = Hybrid {
            space,
            key: dir_key(self.key, space),
            names: None,
            inodes: None,
            identity: None,
            identity_built: false,
            next_seq: self.next_seq,
            next_ids_seq: 1,
            entries: entries as u64,
        };
        let _ = self.copy_rows(&mut h);
        tracing::debug!(entries, "destination index migrated to the store");
        Some(h)
    }

    fn copy_rows(&self, h: &mut Hybrid<'s, 'h>) -> Result<(), FsError> {
        let mut store = h.space.store()?;
        for name in &self.listing {
            store.publish_name(&h.key, name)?;
        }
        for (spelling, target) in &self.owners {
            store.set_owner(&h.key, spelling, target)?;
        }
        for (spelling, seq) in &self.learned {
            store.learn(&h.key, spelling, *seq)?;
        }
        match &self.table {
            None => {
                for p in &self.pending {
                    if let Some(old) = p.replaced {
                        store.ids_row(&h.key, old, None, h.next_ids_seq)?;
                        h.next_ids_seq += 1;
                    }
                    if let FileIdentity::Strong(id) = p.published {
                        for s in &p.spellings {
                            store.ids_row(&h.key, id, Some(s), h.next_ids_seq)?;
                            h.next_ids_seq += 1;
                        }
                    }
                }
            }
            Some(table) => {
                for old in &self.wiped {
                    store.ids_row(&h.key, *old, None, h.next_ids_seq)?;
                    h.next_ids_seq += 1;
                }
                for (id, spellings) in table {
                    for s in spellings {
                        store.ids_row(&h.key, *id, Some(s), h.next_ids_seq)?;
                        h.next_ids_seq += 1;
                    }
                }
            }
        }
        Ok(())
    }
}

impl<'s, 'h> Hybrid<'s, 'h> {
    /// Spec 6.3, steps 1 to 5.
    fn resolve<D: DirHandle>(
        &mut self,
        dir: &D,
        planned: &OsStr,
        tick: &mut Tick<'_>,
    ) -> Result<Result<Resolved, NameError>, FsError> {
        let th = self.space.thresholds();
        // 1. A name this run published or the listing held is itself.
        let mut known = self.space.store()?.has_name(&self.key, planned)?;
        if !known && let Some(names) = &self.names {
            known = names.contains_name(planned.as_encoded_bytes())?;
        }
        if known {
            let meta = dir.metadata(planned)?;
            return Ok(Ok(Resolved::Entry { stored: planned.to_os_string(), meta }));
        }
        // 2. Absent, or an error.
        let meta = match dir.metadata(planned) {
            Ok(meta) => meta,
            Err(e) if is_not_found(&e) => return Ok(Ok(Resolved::Absent)),
            Err(e) => return Err(e),
        };
        // 3. Only a trusted identity can be looked up.
        let FileIdentity::Strong(id) = meta.identity else {
            return Ok(Err(NameError::Unresolvable));
        };
        // 4. The inode hint narrows the directory to a few candidates, each verified by a stat.
        if let Some(inodes) = &self.inodes
            && let Ok(d_ino) = u64::try_from(id.index)
        {
            let tier1 = inodes.names_with_key(&d_ino.to_be_bytes(), th.max_candidates)?;
            if tier1.len() <= th.max_candidates {
                let rows = self.space.store()?.ids_rows(&self.key, id)?;
                let found = Self::candidates(tier1, rows);
                let verified = Self::verify(dir, id, found)?;
                if !verified.is_empty() {
                    return self.pick(verified, meta);
                }
            }
        }
        // 5. The identity family: every name of the directory, stat-ed once.
        self.build_identity(dir, tick)?;
        let tier1 = match &self.identity {
            Some(family) => family.names_with_key(&identity_key(id), usize::MAX)?,
            None => Vec::new(),
        };
        let rows = self.space.store()?.ids_rows(&self.key, id)?;
        let spellings: Vec<OsString> = Self::candidates(tier1, rows).into_keys().collect();
        if spellings.is_empty() {
            return Ok(Err(NameError::Unresolvable));
        }
        self.pick(spellings, meta)
    }

    /// The candidates of an identity: tier 1 names at seq 0 and the `ids` spelling rows at their seq, minus every one
    /// older than the latest wipe row, one entry per name.
    fn candidates(
        tier1: Vec<OsString>,
        rows: Vec<(Option<OsString>, u64)>,
    ) -> BTreeMap<OsString, u64> {
        let latest = rows.iter().filter(|(s, _)| s.is_none()).map(|(_, seq)| *seq).max();
        let mut found: BTreeMap<OsString, u64> = BTreeMap::new();
        let all = tier1
            .into_iter()
            .map(|n| (n, 0))
            .chain(rows.into_iter().filter_map(|(s, seq)| s.map(|s| (s, seq))));
        for (name, seq) in all {
            let slot = found.entry(name).or_insert(0);
            *slot = (*slot).max(seq);
        }
        if let Some(latest) = latest {
            found.retain(|_, seq| *seq >= latest);
        }
        found
    }

    /// Keeps the candidates that still name `id`: another object or a vanished entry drops one, any other error is
    /// the caller's.
    fn verify<D: DirHandle>(
        dir: &D,
        id: ObjectId,
        found: BTreeMap<OsString, u64>,
    ) -> Result<Vec<OsString>, FsError> {
        let mut out = Vec::new();
        for name in found.into_keys() {
            match dir.metadata(&name) {
                Ok(m) if m.identity == FileIdentity::Strong(id) => out.push(name),
                Ok(_) => {}
                Err(e) if is_not_found(&e) => {}
                Err(e) => return Err(e),
            }
        }
        Ok(out)
    }

    fn pick(
        &self,
        spellings: Vec<OsString>,
        meta: Metadata,
    ) -> Result<Result<Resolved, NameError>, FsError> {
        let (space, key) = (self.space, self.key);
        let chosen = choose(&spellings, &|s| space.store()?.learned(&key, s), &|s| {
            space.store()?.owner(&key, s)
        })?;
        Ok(chosen.map(|stored| Resolved::Entry { stored, meta }))
    }

    /// Stats every name of the directory once and keeps `(identity, name)` in a sorted family of its own. A failed
    /// build is not remembered, so the next call retries.
    fn build_identity<D: DirHandle>(
        &mut self,
        dir: &D,
        tick: &mut Tick<'_>,
    ) -> Result<(), FsError> {
        if self.identity_built {
            return Ok(());
        }
        let th = self.space.thresholds();
        let entries = self.entries;
        tracing::debug!(entries, "building the identity family");
        let mut builder = FamilyBuilder::new(self.space, Lane::Identity, 24);
        let mut seen = 0usize;
        let mut visit = |name: OsString| -> Result<(), FsError> {
            seen += 1;
            if th.tick_entries > 0 && seen.is_multiple_of(th.tick_entries) {
                tick()?;
            }
            match dir.metadata(&name) {
                Ok(m) => {
                    if let FileIdentity::Strong(id) = m.identity {
                        let entry = DirEntry { name, file_type: m.file_type, ino: None };
                        builder.push(Keyed { key: identity_key(id), entry }, tick)?;
                    }
                }
                // An entry deleted since it was listed is no longer anyone's name: skipped, not a failure.
                Err(e) if is_not_found(&e) => {}
                Err(e) => return Err(e),
            }
            Ok(())
        };
        if let Some(names) = &self.names {
            for record in names.names() {
                visit(record?.entry.name)?;
            }
        } else {
            self.space.store()?.for_each_name2(&self.key, th.publication_commit, &mut |name| {
                visit(name)?;
                Ok(ControlFlow::Continue(()))
            })?;
        }
        let pending = builder.into_pending()?;
        let family = build_one(self.space, Lane::Identity, pending, tick)?;
        tracing::debug!(entries, "identity family built");
        self.identity = family;
        self.identity_built = true;
        Ok(())
    }

    /// Writes one publication to tier 2. Every write error poisons the space (the store does it), so the next call
    /// that reads the index returns it.
    fn record(
        &mut self,
        target: &FluxPathKey,
        stored: Option<&OsStr>,
        planned: &OsStr,
        replaced: Option<ObjectId>,
        published: FileIdentity,
    ) {
        let _ = self.write(target, stored, planned, replaced, published);
    }

    fn write(
        &mut self,
        target: &FluxPathKey,
        stored: Option<&OsStr>,
        planned: &OsStr,
        replaced: Option<ObjectId>,
        published: FileIdentity,
    ) -> Result<(), FsError> {
        let mut spellings = Vec::with_capacity(2);
        spellings.extend(stored);
        if !spellings.contains(&planned) {
            spellings.push(planned);
        }
        let mut store = self.space.store()?;
        for s in &spellings {
            store.publish_name(&self.key, s)?;
            store.set_owner(&self.key, s, target)?;
            self.entries += 1;
        }
        if let Some(s) = stored
            && store.learned(&self.key, s)?.is_none()
        {
            store.learn(&self.key, s, self.next_seq)?;
            self.next_seq += 1;
        }
        if let Some(old) = replaced {
            store.ids_row(&self.key, old, None, self.next_ids_seq)?;
            self.next_ids_seq += 1;
        }
        if let FileIdentity::Strong(id) = published {
            for s in &spellings {
                store.ids_row(&self.key, id, Some(s), self.next_ids_seq)?;
                self.next_ids_seq += 1;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fault_fs::FaultFs;
    use flux_fs::{Code, DestinationRoot, FileSystem};
    use std::ffi::OsStr;
    use std::path::Path;

    fn fs_with(names: &[&str]) -> FaultFs {
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/d")).unwrap();
        for n in names {
            fs.write_file(Path::new("/d").join(n), b"x");
        }
        fs.set_case_insensitive(true);
        fs
    }

    fn strong(fs: &FaultFs, path: &str) -> ObjectId {
        match fs.metadata(Path::new(path)).unwrap().identity {
            FileIdentity::Strong(id) => id,
            other => panic!("not strong: {other:?}"),
        }
    }

    /// The call log with `\` read as `/`: the fake joins paths with `PathBuf::join`, so on Windows an entry reads
    /// `metadata(/d\FILE.TXT)`.
    fn calls_norm(fs: &FaultFs) -> Vec<String> {
        fs.calls().into_iter().map(|c| c.replace('\\', "/")).collect()
    }

    fn key(s: &str) -> FluxPathKey {
        FluxPathKey(s.as_bytes().to_vec())
    }

    fn stored(r: Result<Result<Resolved, NameError>, flux_fs::FsError>) -> OsString {
        match r.unwrap().unwrap() {
            Resolved::Entry { stored, .. } => stored,
            Resolved::Absent => panic!("absent"),
        }
    }

    fn record_abc(idx: &mut NameIndex<'_, '_>) {
        for n in ["a", "b", "c"] {
            idx.record_publication(&key(n), None, OsStr::new(n), None, FileIdentity::Unavailable);
        }
    }

    #[test]
    fn a_created_dirs_index_records_nothing() {
        let mut idx = NameIndex::for_created_dir();
        record_abc(&mut idx);
        assert!(idx.inline().listing.is_empty());
        assert!(idx.inline().owners.is_empty());
        assert!(idx.inline().pending.is_empty());
        assert!(!idx.is_tracked());
    }

    #[test]
    fn a_dirs_index_that_is_not_fresh_still_records() {
        let mut idx = NameIndex::for_new_dir();
        record_abc(&mut idx);
        assert_eq!(idx.inline().listing.len(), 3);
        assert_eq!(idx.owner(OsStr::new("b")).unwrap(), Some(key("b")));
        assert_eq!(idx.inline().pending.len(), 3);
        assert!(idx.is_tracked());
    }

    // The production call sites pass `Some(&d).filter(..)`, which is what lets `D` be inferred.
    #[allow(clippy::some_filter)]
    #[test]
    fn for_dir_decides_by_fresh_and_never_reads_for_a_fresh_or_unlisted_dir() {
        let fs = fs_with(&["x", "y"]);
        let d = fs.destination_root(Path::new("/d")).unwrap();
        let fresh = NameIndex::for_dir(true, Some(&d), None, None).unwrap();
        assert!(!fresh.is_tracked());
        assert!(fresh.inline().listing.is_empty());
        let unlisted = NameIndex::for_dir(false, Some(&d).filter(|_| false), None, None).unwrap();
        assert!(unlisted.is_tracked());
        assert!(unlisted.inline().listing.is_empty());
        let listed = NameIndex::for_dir(false, Some(&d), None, None).unwrap();
        assert!(listed.is_tracked());
        let want: BTreeSet<OsString> = ["x", "y"].iter().map(OsString::from).collect();
        assert_eq!(listed.inline().listing, want);
        let reads = calls_norm(&fs).iter().filter(|c| c.starts_with("read_dir(")).count();
        assert_eq!(reads, 1);
    }

    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "an untracked index is never read")]
    fn a_created_dirs_index_is_never_resolved() {
        let fs = fs_with(&["x"]);
        let d = fs.destination_root(Path::new("/d")).unwrap();
        let _ = NameIndex::for_created_dir().resolve(&d, OsStr::new("x"), &mut || Ok(()));
    }

    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "an untracked index is never read")]
    fn a_created_dirs_index_is_never_asked_for_an_owner() {
        let _ = NameIndex::for_created_dir().owner(OsStr::new("x"));
    }

    #[test]
    fn an_exact_name_resolves_to_itself() {
        let fs = fs_with(&["file.txt"]);
        let d = fs.destination_root(Path::new("/d")).unwrap();
        let mut idx = NameIndex::for_existing(&d).unwrap();
        assert_eq!(stored(idx.resolve(&d, OsStr::new("file.txt"), &mut || Ok(()))), "file.txt");
    }

    #[test]
    fn an_absent_name_is_absent() {
        let fs = fs_with(&["file.txt"]);
        let d = fs.destination_root(Path::new("/d")).unwrap();
        let mut idx = NameIndex::for_existing(&d).unwrap();
        assert_eq!(
            idx.resolve(&d, OsStr::new("other.txt"), &mut || Ok(())).unwrap(),
            Ok(Resolved::Absent)
        );
    }

    #[test]
    fn another_spelling_resolves_by_identity_to_the_stored_name() {
        let fs = fs_with(&["FILE.TXT", "other.txt"]);
        let d = fs.destination_root(Path::new("/d")).unwrap();
        let mut idx = NameIndex::for_existing(&d).unwrap();
        assert_eq!(stored(idx.resolve(&d, OsStr::new("file.txt"), &mut || Ok(()))), "FILE.TXT");
    }

    #[test]
    fn the_table_is_built_once_per_directory() {
        let fs = fs_with(&["FILE.TXT", "OTHER.TXT"]);
        let d = fs.destination_root(Path::new("/d")).unwrap();
        let mut idx = NameIndex::for_existing(&d).unwrap();
        assert_eq!(stored(idx.resolve(&d, OsStr::new("file.txt"), &mut || Ok(()))), "FILE.TXT");
        assert_eq!(stored(idx.resolve(&d, OsStr::new("other.txt"), &mut || Ok(()))), "OTHER.TXT");
        let count = |p: &str| calls_norm(&fs).iter().filter(|c| c.as_str() == p).count();
        // Each listing entry is stat-ed once for the table; the planned names are stat-ed once per resolution
        // (the case-insensitive fake logs the normalized stored spelling for both).
        assert_eq!(count("metadata(/d/FILE.TXT)"), 2); // table + planned `file.txt`
        assert_eq!(count("metadata(/d/OTHER.TXT)"), 2); // table + planned `other.txt`
        assert_eq!(calls_norm(&fs).iter().filter(|c| c.starts_with("read_dir(")).count(), 1);
    }

    #[test]
    fn a_hardlink_alias_in_the_listing_is_unresolvable() {
        let fs = fs_with(&["a.txt", "b.txt"]);
        let id = fs.metadata(Path::new("/d/a.txt")).unwrap().identity;
        fs.set_identity("/d/b.txt", id);
        let d = fs.destination_root(Path::new("/d")).unwrap();
        let mut idx = NameIndex::for_existing(&d).unwrap();
        assert_eq!(
            idx.resolve(&d, OsStr::new("A.TXT"), &mut || Ok(())).unwrap(),
            Err(NameError::Unresolvable)
        );
    }

    #[test]
    fn a_weak_identity_is_unresolvable() {
        let fs = fs_with(&["FILE.TXT"]);
        let id = strong(&fs, "/d/FILE.TXT");
        fs.set_identity("/d/FILE.TXT", FileIdentity::Weak(id));
        let d = fs.destination_root(Path::new("/d")).unwrap();
        let mut idx = NameIndex::for_existing(&d).unwrap();
        assert_eq!(
            idx.resolve(&d, OsStr::new("file.txt"), &mut || Ok(())).unwrap(),
            Err(NameError::Unresolvable)
        );
        fs.set_identity("/d/FILE.TXT", FileIdentity::Unavailable);
        assert_eq!(
            idx.resolve(&d, OsStr::new("file.txt"), &mut || Ok(())).unwrap(),
            Err(NameError::Unresolvable)
        );
    }

    #[test]
    fn after_a_replace_the_new_object_resolves_through_both_spellings() {
        let fs = fs_with(&["FILE.TXT"]);
        let old = strong(&fs, "/d/FILE.TXT");
        let d = fs.destination_root(Path::new("/d")).unwrap();
        let mut idx = NameIndex::for_existing(&d).unwrap();
        // Build the table before the publication, so the update path (not the lazy build) is what is measured.
        assert_eq!(stored(idx.resolve(&d, OsStr::new("file.txt"), &mut || Ok(()))), "FILE.TXT");
        // Windows-style: the replace renames, so only `File.txt` is on disk afterwards.
        fs.set_replace_renames(true);
        fs.write_file("/d/src", b"new");
        fs.rename_replace(Path::new("/d/src"), Path::new("/d/File.txt")).unwrap();
        let new = strong(&fs, "/d/File.txt");
        assert_ne!(old, new);
        idx.record_publication(
            &key("file"),
            Some(OsStr::new("FILE.TXT")),
            OsStr::new("File.txt"),
            Some(old),
            FileIdentity::Strong(new),
        );
        // A third spelling: two recorded spellings, one owner, one entry; the pre-publication spelling is the key.
        assert_eq!(stored(idx.resolve(&d, OsStr::new("fIlE.tXt"), &mut || Ok(()))), "FILE.TXT");
        // The replaced object's identity is gone from the table.
        assert!(!idx.inline().table.as_ref().unwrap().contains_key(&old));
    }

    #[test]
    fn two_spellings_of_different_targets_are_ambiguous() {
        let fs = fs_with(&["FILE.TXT"]);
        let id = strong(&fs, "/d/FILE.TXT");
        let d = fs.destination_root(Path::new("/d")).unwrap();
        let mut idx = NameIndex::for_existing(&d).unwrap();
        idx.record_publication(
            &key("a"),
            Some(OsStr::new("FILE.TXT")),
            OsStr::new("File.txt"),
            None,
            FileIdentity::Strong(id),
        );
        // A second target claims another spelling of the same object.
        idx.record_publication(
            &key("b"),
            None,
            OsStr::new("file.TXT"),
            None,
            FileIdentity::Strong(id),
        );
        assert_eq!(
            idx.resolve(&d, OsStr::new("fILE.txt"), &mut || Ok(())).unwrap(),
            Err(NameError::Unresolvable)
        );
    }

    #[test]
    fn a_new_dirs_index_has_an_empty_listing_and_no_reads() {
        let fs = fs_with(&[]);
        let idx = NameIndex::for_new_dir();
        assert!(idx.inline().listing.is_empty());
        assert!(calls_norm(&fs).iter().all(|c| !c.starts_with("read_dir(")));
    }

    #[test]
    fn a_metadata_error_other_than_not_found_is_returned_as_the_outer_error() {
        let fs = fs_with(&["FILE.TXT"]);
        let d = fs.destination_root(Path::new("/d")).unwrap();
        let mut idx = NameIndex::for_existing(&d).unwrap();
        fs.fail_kind("metadata", Code::IoError, std::io::ErrorKind::PermissionDenied);
        let e = idx.resolve(&d, OsStr::new("file.txt"), &mut || Ok(())).unwrap_err();
        assert_eq!(e.source.kind(), std::io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn a_listing_entry_that_disappears_during_table_building_is_skipped_not_fatal() {
        // The 2nd `metadata` call is the first listing entry's, during the table build (the 1st is the planned name's).
        // NotFound there is an entry deleted since the listing: skipped, and the name is then simply unresolvable.
        let fs = fs_with(&["FILE.TXT"]);
        let d = fs.destination_root(Path::new("/d")).unwrap();
        let mut idx = NameIndex::for_existing(&d).unwrap();
        fs.fail_nth("metadata", 2, Code::IoError, std::io::ErrorKind::NotFound);
        assert_eq!(
            idx.resolve(&d, OsStr::new("file.txt"), &mut || Ok(())).unwrap(),
            Err(NameError::Unresolvable)
        );
        // Any other error while building is a real fault and propagates.
        let fs = fs_with(&["FILE.TXT"]);
        let d = fs.destination_root(Path::new("/d")).unwrap();
        let mut idx = NameIndex::for_existing(&d).unwrap();
        fs.fail_nth("metadata", 2, Code::IoError, std::io::ErrorKind::PermissionDenied);
        let e = idx.resolve(&d, OsStr::new("file.txt"), &mut || Ok(())).unwrap_err();
        assert_eq!(e.source.kind(), std::io::ErrorKind::PermissionDenied);
        // And a failed build is not remembered: the next call retries and succeeds.
        assert_eq!(stored(idx.resolve(&d, OsStr::new("file.txt"), &mut || Ok(()))), "FILE.TXT");
    }

    #[test]
    fn resolve_does_not_mutate_the_directory() {
        let fs = fs_with(&["FILE.TXT"]);
        let d = fs.destination_root(Path::new("/d")).unwrap();
        let mut idx = NameIndex::for_existing(&d).unwrap();
        let before = fs.calls().len(); // the fixture's own create_dir / read_dir are not under test
        idx.resolve(&d, OsStr::new("file.txt"), &mut || Ok(())).unwrap().unwrap();
        idx.resolve(&d, OsStr::new("nope"), &mut || Ok(())).unwrap().unwrap();
        let calls = calls_norm(&fs).split_off(before);
        assert!(!calls.is_empty());
        assert!(calls.iter().all(|c| c.starts_with("metadata(")), "{calls:?}");
        assert_eq!(fs.read_file("/d/FILE.TXT"), Some(b"x".to_vec()));
    }

    #[test]
    fn a_weak_planned_entry_is_unresolvable_even_when_a_strong_listing_entry_shares_its_object_id()
    {
        let fs = fs_with(&["a.txt"]);
        let id = strong(&fs, "/d/a.txt");
        let d = fs.destination_root(Path::new("/d")).unwrap();
        let mut idx = NameIndex::for_existing(&d).unwrap();
        // Created after the listing was read: not in the listing, so it is resolved by identity - and its Weak
        // identity names the same object id as the Strong listing entry.
        fs.write_file("/d/late.txt", b"x");
        fs.set_identity("/d/late.txt", FileIdentity::Weak(id));
        assert_eq!(
            idx.resolve(&d, OsStr::new("late.txt"), &mut || Ok(())).unwrap(),
            Err(NameError::Unresolvable)
        );
    }

    #[test]
    fn the_stored_spelling_does_not_depend_on_whether_the_table_was_built_before_the_publication() {
        let publish = |idx: &mut NameIndex<'_, '_>, id: ObjectId| {
            idx.record_publication(
                &key("t"),
                Some(OsStr::new("file.txt")),
                OsStr::new("File.txt"),
                None,
                FileIdentity::Strong(id),
            );
        };
        // Built before: resolve once (by identity, via a third spelling) to build the table, then publish.
        let fs = fs_with(&["file.txt"]);
        let id = strong(&fs, "/d/file.txt");
        let d = fs.destination_root(Path::new("/d")).unwrap();
        let mut before = NameIndex::for_existing(&d).unwrap();
        assert_eq!(stored(before.resolve(&d, OsStr::new("FILE.TXT"), &mut || Ok(()))), "file.txt");
        publish(&mut before, id);
        // Built after: publish first; the lazy build then sees both spellings in the listing.
        let mut after = NameIndex::for_existing(&d).unwrap();
        publish(&mut after, id);
        let (a, b) = (
            stored(before.resolve(&d, OsStr::new("FILE.txt"), &mut || Ok(()))),
            stored(after.resolve(&d, OsStr::new("FILE.txt"), &mut || Ok(()))),
        );
        assert_eq!(a, "file.txt");
        assert_eq!(a, b);
    }

    #[test]
    fn learned_spellings_are_deduplicated_and_sequenced_so_resolution_scans_only_the_candidates() {
        // A wall-clock assertion would be flaky, so the O(1) property is pinned structurally: each spelling is
        // inserted once with a sequence number, a repeat does not grow the map, and the choice among a handful of
        // candidate spellings never walks the 20_000 learned entries.
        let fs = fs_with(&["file.txt"]);
        let id = strong(&fs, "/d/file.txt");
        let d = fs.destination_root(Path::new("/d")).unwrap();
        let mut idx = NameIndex::for_existing(&d).unwrap();
        for i in 0..20_000 {
            let s = format!("other{i}");
            idx.record_publication(
                &key(&s),
                Some(OsStr::new(&s)),
                OsStr::new(&s),
                None,
                FileIdentity::Unavailable,
            );
        }
        idx.record_publication(
            &key("t"),
            Some(OsStr::new("file.txt")),
            OsStr::new("File.txt"),
            None,
            FileIdentity::Strong(id),
        );
        idx.record_publication(
            &key("t"),
            Some(OsStr::new("file.txt")),
            OsStr::new("File.txt"),
            None,
            FileIdentity::Strong(id),
        );
        assert_eq!(idx.inline().learned.len(), 20_001);
        assert_eq!(idx.inline().next_seq, 20_001);
        assert_eq!(stored(idx.resolve(&d, OsStr::new("FILE.txt"), &mut || Ok(()))), "file.txt");
    }

    // ---------------------------------------------------------------------------------------------------------
    // The hybrid form (plan 3, task 5).

    use crate::scratch::{Lane, Thresholds};
    use crate::store::{DirKey, faults};
    use flux_fs::ObjectId;
    use std::path::PathBuf;

    type Dir = <FaultFs as DestinationRoot>::Dir;

    /// `index_inline` 4, `max_candidates` 2, `tick_entries` 3, `publication_commit` 2, 128-byte blocks.
    fn tiny() -> Thresholds {
        Thresholds::new(4, 8, 2, 16, 3).unwrap().with_index(4, 2, 4, 1 << 20, 2, 128).unwrap()
    }

    fn space_on(fs: &FaultFs) -> ScratchSpace<'static> {
        fs.create_dir(Path::new("/s")).unwrap();
        let home = fs.destination_root(Path::new("/s")).unwrap();
        let space = ScratchSpace::unplaced(tiny(), None);
        space.place(home, PathBuf::from("/s"));
        space
    }

    /// `n` filler names in `/d`.
    fn big_dir(fs: &FaultFs, n: usize) {
        for i in 0..n {
            fs.write_file(format!("/d/pad{i}"), b"x");
        }
    }

    fn os(s: &str) -> &OsStr {
        OsStr::new(s)
    }

    fn show(r: Result<Result<Resolved, NameError>, flux_fs::FsError>) -> String {
        match r {
            Ok(Ok(Resolved::Entry { stored, .. })) => format!("entry {}", stored.to_string_lossy()),
            Ok(Ok(Resolved::Absent)) => String::from("absent"),
            Ok(Err(NameError::Unresolvable)) => String::from("unresolvable"),
            Err(e) => format!("err {:?}", e.source.kind()),
        }
    }

    fn probe(idx: &mut NameIndex<'_, '_>, d: &Dir, name: &str) -> String {
        show(idx.resolve(d, os(name), &mut || Ok(())))
    }

    fn hybrid_over<'s>(
        space: &'s ScratchSpace<'static>,
        d: &Dir,
        key: Option<ObjectId>,
    ) -> NameIndex<'s, 'static> {
        let mut t = || Ok(());
        let idx = NameIndex::for_dir(
            false,
            Some(d),
            Some(IndexSite { scratch: space, tick: &mut t }),
            key,
        )
        .unwrap_or_else(|e| panic!("{e:?}"));
        assert!(idx.is_hybrid());
        idx
    }

    /// Runs the scenario over the same directory once with the in-memory index and once with the hybrid one, and
    /// asserts the two transcripts are the same. Returns the transcript.
    fn both_with(
        files: &[&str],
        mode: crate::fault_fs::InoMode,
        setup: impl Fn(&FaultFs),
        scenario: impl Fn(&mut NameIndex<'_, '_>, &FaultFs, &Dir) -> Vec<String>,
    ) -> Vec<String> {
        let pads = 6usize.saturating_sub(files.len());
        let mut inline_run = Vec::new();
        let mut hybrid_run = Vec::new();
        for hybrid in [false, true] {
            let fs = fs_with(files);
            big_dir(&fs, pads);
            fs.set_ino_mode(Path::new("/d"), mode);
            setup(&fs);
            let d = fs.destination_root(Path::new("/d")).unwrap();
            if hybrid {
                let space = space_on(&fs);
                let mut idx = hybrid_over(&space, &d, None);
                hybrid_run = scenario(&mut idx, &fs, &d);
            } else {
                let mut idx = NameIndex::for_dir(false, Some(&d), None, None).unwrap();
                inline_run = scenario(&mut idx, &fs, &d);
            }
        }
        assert_eq!(
            inline_run, hybrid_run,
            "the hybrid index answered differently from the inline one"
        );
        inline_run
    }

    fn both(
        files: &[&str],
        scenario: impl Fn(&mut NameIndex<'_, '_>, &FaultFs, &Dir) -> Vec<String>,
    ) -> Vec<String> {
        both_with(files, crate::fault_fs::InoMode::Real, |_| {}, scenario)
    }

    /// Replaces the file at `/d/<name>` with a new object (the Windows way: the replace renames) and returns its id.
    fn replace_object(fs: &FaultFs, name: &str) -> ObjectId {
        fs.set_replace_renames(true);
        fs.write_file("/d/replacement-src", b"new");
        fs.rename_replace(Path::new("/d/replacement-src"), &Path::new("/d").join(name)).unwrap();
        strong(fs, &format!("/d/{name}"))
    }

    fn publish(
        idx: &mut NameIndex<'_, '_>,
        t: &str,
        stored: Option<&str>,
        planned: &str,
        replaced: Option<ObjectId>,
        published: FileIdentity,
    ) {
        idx.record_publication(&key(t), stored.map(os), os(planned), replaced, published);
    }

    // The scenarios of the cut 8b tests, answer-only, through the equivalence harness.

    #[test]
    fn an_exact_name_resolves_to_itself_in_both_forms() {
        let t = both(&["file.txt"], |idx, _, d| vec![probe(idx, d, "file.txt")]);
        assert_eq!(t, ["entry file.txt"]);
    }

    #[test]
    fn an_absent_name_is_absent_in_both_forms() {
        let t = both(&["file.txt"], |idx, _, d| vec![probe(idx, d, "other.txt")]);
        assert_eq!(t, ["absent"]);
    }

    #[test]
    fn another_spelling_resolves_by_identity_in_both_forms() {
        let t = both(&["FILE.TXT", "other.txt"], |idx, _, d| vec![probe(idx, d, "file.txt")]);
        assert_eq!(t, ["entry FILE.TXT"]);
    }

    #[test]
    fn a_hardlink_alias_in_the_listing_is_unresolvable_in_both_forms() {
        let t = both_with(
            &["a.txt", "b.txt"],
            crate::fault_fs::InoMode::Real,
            |fs| {
                let id = fs.metadata(Path::new("/d/a.txt")).unwrap().identity;
                fs.set_identity("/d/b.txt", id);
            },
            |idx, _, d| vec![probe(idx, d, "A.TXT")],
        );
        assert_eq!(t, ["unresolvable"]);
    }

    #[test]
    fn a_weak_identity_is_unresolvable_in_both_forms() {
        let t = both(&["FILE.TXT"], |idx, fs, d| {
            let id = strong(fs, "/d/FILE.TXT");
            fs.set_identity("/d/FILE.TXT", FileIdentity::Weak(id));
            let first = probe(idx, d, "file.txt");
            fs.set_identity("/d/FILE.TXT", FileIdentity::Unavailable);
            vec![first, probe(idx, d, "file.txt")]
        });
        assert_eq!(t, ["unresolvable", "unresolvable"]);
    }

    #[test]
    fn after_a_replace_the_new_object_resolves_through_both_spellings_in_both_forms() {
        let t = both(&["FILE.TXT"], |idx, fs, d| {
            let old = strong(fs, "/d/FILE.TXT");
            let before = probe(idx, d, "file.txt");
            let new = replace_object(fs, "File.txt");
            publish(
                idx,
                "file",
                Some("FILE.TXT"),
                "File.txt",
                Some(old),
                FileIdentity::Strong(new),
            );
            vec![before, probe(idx, d, "fIlE.tXt")]
        });
        assert_eq!(t, ["entry FILE.TXT", "entry FILE.TXT"]);
    }

    #[test]
    fn two_spellings_of_different_targets_are_ambiguous_in_both_forms() {
        let t = both(&["FILE.TXT"], |idx, fs, d| {
            let id = strong(fs, "/d/FILE.TXT");
            publish(idx, "a", Some("FILE.TXT"), "File.txt", None, FileIdentity::Strong(id));
            publish(idx, "b", None, "file.TXT", None, FileIdentity::Strong(id));
            vec![probe(idx, d, "fILE.txt")]
        });
        assert_eq!(t, ["unresolvable"]);
    }

    #[test]
    fn a_metadata_error_other_than_not_found_is_the_outer_error_in_both_forms() {
        let t = both(&["FILE.TXT"], |idx, fs, d| {
            fs.fail_kind("metadata", Code::IoError, std::io::ErrorKind::PermissionDenied);
            vec![probe(idx, d, "file.txt")]
        });
        assert_eq!(t, ["err PermissionDenied"]);
    }

    #[test]
    fn a_weak_planned_entry_is_unresolvable_in_both_forms() {
        let t = both(&["a.txt"], |idx, fs, d| {
            let id = strong(fs, "/d/a.txt");
            fs.write_file("/d/late.txt", b"x");
            fs.set_identity("/d/late.txt", FileIdentity::Weak(id));
            vec![probe(idx, d, "late.txt")]
        });
        assert_eq!(t, ["unresolvable"]);
    }

    #[test]
    fn the_stored_spelling_does_not_depend_on_when_the_table_was_built_in_both_forms() {
        let t = both(&["file.txt"], |idx, fs, d| {
            let id = strong(fs, "/d/file.txt");
            let first = probe(idx, d, "FILE.TXT");
            publish(idx, "t", Some("file.txt"), "File.txt", None, FileIdentity::Strong(id));
            vec![first, probe(idx, d, "FILE.txt")]
        });
        assert_eq!(t, ["entry file.txt", "entry file.txt"]);
    }

    #[test]
    fn learned_spellings_pick_the_first_learned_in_both_forms() {
        let t = both(&["file.txt"], |idx, fs, d| {
            let id = strong(fs, "/d/file.txt");
            for i in 0..50 {
                let s = format!("other{i}");
                publish(idx, &s, Some(&s), &s, None, FileIdentity::Unavailable);
            }
            publish(idx, "t", Some("file.txt"), "File.txt", None, FileIdentity::Strong(id));
            publish(idx, "t", Some("file.txt"), "File.txt", None, FileIdentity::Strong(id));
            vec![probe(idx, d, "FILE.txt")]
        });
        assert_eq!(t, ["entry file.txt"]);
    }

    #[test]
    fn owners_agree_in_both_forms() {
        let t = both(&["a"], |idx, _, _| {
            publish(idx, "t1", None, "x", None, FileIdentity::Unavailable);
            publish(idx, "t2", Some("y"), "Y", None, FileIdentity::Unavailable);
            ["x", "y", "Y", "nope"]
                .iter()
                .map(|n| format!("{:?}", idx.owner(os(n)).unwrap()))
                .collect()
        });
        assert_eq!(t[0], format!("{:?}", Some(key("t1"))));
        assert_eq!(t[3], "None");
    }

    // The inline-only tests keep their hybrid twins.

    fn count_metadata(fs: &FaultFs) -> usize {
        fs.calls().iter().filter(|c| c.starts_with("metadata(")).count()
    }

    #[test]
    fn the_identity_family_is_built_once_per_directory() {
        let fs = fs_with(&["FILE.TXT", "OTHER.TXT"]);
        big_dir(&fs, 4);
        fs.set_ino_mode(Path::new("/d"), crate::fault_fs::InoMode::Absent);
        let d = fs.destination_root(Path::new("/d")).unwrap();
        let space = space_on(&fs);
        let mut idx = hybrid_over(&space, &d, None);
        assert!(!idx.identity_built());
        let before = count_metadata(&fs);
        assert_eq!(probe(&mut idx, &d, "file.txt"), "entry FILE.TXT");
        assert!(idx.identity_built());
        assert_eq!(probe(&mut idx, &d, "other.txt"), "entry OTHER.TXT");
        // Six names stat-ed once for the family, plus the one stat of each planned name.
        assert_eq!(count_metadata(&fs) - before, 6 + 2);
    }

    #[test]
    fn a_name_that_disappears_during_the_identity_pass_is_skipped() {
        let fs = fs_with(&["FILE.TXT", "OTHER.TXT"]);
        big_dir(&fs, 4);
        fs.set_ino_mode(Path::new("/d"), crate::fault_fs::InoMode::Absent);
        let d = fs.destination_root(Path::new("/d")).unwrap();
        let space = space_on(&fs);
        let mut idx = hybrid_over(&space, &d, None);
        // The 2nd `metadata` call is the first name of the pass (`FILE.TXT` sorts first); the 1st is the planned name's.
        fs.fail_nth("metadata", 2, Code::IoError, std::io::ErrorKind::NotFound);
        assert_eq!(probe(&mut idx, &d, "file.txt"), "unresolvable");
        // Any other error is a real fault: it propagates and the family stays unbuilt, so the next call retries.
        let fs = fs_with(&["FILE.TXT", "OTHER.TXT"]);
        big_dir(&fs, 4);
        fs.set_ino_mode(Path::new("/d"), crate::fault_fs::InoMode::Absent);
        let d = fs.destination_root(Path::new("/d")).unwrap();
        let space = space_on(&fs);
        let mut idx = hybrid_over(&space, &d, None);
        fs.fail_nth("metadata", 2, Code::IoError, std::io::ErrorKind::PermissionDenied);
        assert_eq!(probe(&mut idx, &d, "file.txt"), "err PermissionDenied");
        assert!(!idx.identity_built());
        assert_eq!(probe(&mut idx, &d, "file.txt"), "entry FILE.TXT");
        assert!(idx.identity_built());
    }

    // Spec 6.3 step 5.

    #[test]
    fn a_wrong_d_ino_costs_the_identity_pass_never_a_wrong_answer() {
        let t = both_with(
            &["FILE.TXT", "other.txt"],
            crate::fault_fs::InoMode::Wrong,
            |_| {},
            |idx, _, d| vec![probe(idx, d, "file.txt")],
        );
        assert_eq!(t[0], "entry FILE.TXT");
        let fs = fs_with(&["FILE.TXT", "other.txt"]);
        big_dir(&fs, 4);
        fs.set_ino_mode(Path::new("/d"), crate::fault_fs::InoMode::Wrong);
        let d = fs.destination_root(Path::new("/d")).unwrap();
        let space = space_on(&fs);
        let mut idx = hybrid_over(&space, &d, None);
        assert_eq!(probe(&mut idx, &d, "file.txt"), "entry FILE.TXT");
        assert!(idx.identity_built());
    }

    #[test]
    fn a_replacement_published_before_the_identity_family_is_built() {
        let t = both_with(
            &["FILE.TXT"],
            crate::fault_fs::InoMode::Absent,
            |_| {},
            |idx, fs, d| {
                let old = strong(fs, "/d/FILE.TXT");
                let new = replace_object(fs, "File.txt");
                publish(
                    idx,
                    "t",
                    Some("FILE.TXT"),
                    "File.txt",
                    Some(old),
                    FileIdentity::Strong(new),
                );
                vec![probe(idx, d, "fIlE.tXt")]
            },
        );
        assert_eq!(t, ["entry FILE.TXT"]);
    }

    #[test]
    fn a_replacement_published_after_it() {
        let t = both_with(
            &["FILE.TXT"],
            crate::fault_fs::InoMode::Absent,
            |_| {},
            |idx, fs, d| {
                let old = strong(fs, "/d/FILE.TXT");
                let first = probe(idx, d, "file.txt");
                let new = replace_object(fs, "File.txt");
                publish(
                    idx,
                    "t",
                    Some("FILE.TXT"),
                    "File.txt",
                    Some(old),
                    FileIdentity::Strong(new),
                );
                vec![first, probe(idx, d, "fIlE.tXt")]
            },
        );
        assert_eq!(t, ["entry FILE.TXT", "entry FILE.TXT"]);
    }

    #[test]
    fn a_name_replaced_and_replaced_back() {
        let t = both_with(
            &["FILE.TXT"],
            crate::fault_fs::InoMode::Absent,
            |_| {},
            |idx, fs, d| {
                let x = strong(fs, "/d/FILE.TXT");
                let first = probe(idx, d, "file.txt");
                let y = replace_object(fs, "FILE.TXT");
                publish(idx, "t", Some("FILE.TXT"), "FILE.TXT", Some(x), FileIdentity::Strong(y));
                let z = replace_object(fs, "FILE.TXT");
                publish(idx, "t", Some("FILE.TXT"), "FILE.TXT", Some(y), FileIdentity::Strong(z));
                vec![first, probe(idx, d, "file.txt")]
            },
        );
        assert_eq!(t, ["entry FILE.TXT", "entry FILE.TXT"]);
    }

    #[test]
    fn a_replaced_object_with_a_second_spelling_loses_both() {
        let t = both_with(
            &["a.txt", "b.txt"],
            crate::fault_fs::InoMode::Real,
            |fs| {
                let id = fs.metadata(Path::new("/d/a.txt")).unwrap().identity;
                fs.set_identity("/d/b.txt", id);
            },
            |idx, fs, d| {
                let x = strong(fs, "/d/a.txt");
                // Both listed spellings of X are published by the target that then replaces X.
                publish(idx, "t", Some("a.txt"), "a.txt", None, FileIdentity::Strong(x));
                publish(idx, "t", Some("b.txt"), "b.txt", None, FileIdentity::Strong(x));
                let y = replace_object(fs, "a.txt");
                publish(idx, "t", Some("a.txt"), "a.txt", Some(x), FileIdentity::Strong(y));
                fs.remove_file(Path::new("/d/b.txt")).unwrap();
                vec![probe(idx, d, "A.TXT"), probe(idx, d, "B.TXT")]
            },
        );
        assert_eq!(t, ["entry a.txt", "absent"]);
    }

    #[test]
    fn a_new_file_met_under_a_folded_alias_with_no_inodes_family() {
        let t = both_with(
            &["a.txt"],
            crate::fault_fs::InoMode::Absent,
            |_| {},
            |idx, fs, d| {
                fs.write_file("/d/Late.txt", b"x");
                let id = strong(fs, "/d/Late.txt");
                publish(idx, "t", None, "Late.txt", None, FileIdentity::Strong(id));
                vec![probe(idx, d, "late.TXT")]
            },
        );
        assert_eq!(t, ["entry Late.txt"]);
    }

    #[test]
    fn a_publications_stored_and_planned_spellings_share_a_d_ino() {
        let t = both(&["FILE.TXT"], |idx, fs, d| {
            let id = strong(fs, "/d/FILE.TXT");
            publish(idx, "t", Some("FILE.TXT"), "File.txt", None, FileIdentity::Strong(id));
            vec![probe(idx, d, "fIlE.tXt")]
        });
        assert_eq!(t, ["entry FILE.TXT"]);
    }

    #[test]
    fn a_surviving_hardlink_spelling_of_a_replaced_object_is_unresolvable_under_an_alias() {
        let t = both_with(
            &["a.txt", "b.txt"],
            crate::fault_fs::InoMode::Real,
            |fs| {
                let id = fs.metadata(Path::new("/d/a.txt")).unwrap().identity;
                fs.set_identity("/d/b.txt", id);
            },
            |idx, fs, d| {
                let x = strong(fs, "/d/a.txt");
                let y = replace_object(fs, "a.txt");
                publish(idx, "t", Some("a.txt"), "a.txt", Some(x), FileIdentity::Strong(y));
                vec![probe(idx, d, "B.TXT")]
            },
        );
        assert_eq!(t, ["unresolvable"]);
    }

    // Migration (P1).

    fn new_file(fs: &FaultFs, name: &str) -> FileIdentity {
        fs.write_file(format!("/d/{name}"), b"x");
        FileIdentity::Strong(strong(fs, &format!("/d/{name}")))
    }

    #[test]
    fn a_never_listed_index_migrates_at_the_threshold_and_still_agrees() {
        let fs = fs_with(&["a"]);
        let d = fs.destination_root(Path::new("/d")).unwrap();
        let space = space_on(&fs);
        let mut migrating = NameIndex::for_new_dir_with(Some(&space), None);
        let mut reference = NameIndex::for_new_dir_with(None, None);
        for i in 0..5 {
            let name = format!("n{i}");
            let id = new_file(&fs, &name);
            for idx in [&mut migrating, &mut reference] {
                publish(idx, &name, None, &name, None, id);
            }
            assert_eq!(migrating.is_hybrid(), i == 4, "after publication {}", i + 1);
        }
        for alias in ["N0", "N3", "N4", "nope"] {
            assert_eq!(
                probe(&mut migrating, &d, alias),
                probe(&mut reference, &d, alias),
                "{alias}"
            );
        }
        assert_eq!(probe(&mut migrating, &d, "N0"), "entry n0");
        assert_eq!(migrating.owner(os("n2")).unwrap(), Some(key("n2")));
    }

    #[test]
    fn a_listed_inline_index_migrates_when_publications_push_it_over() {
        let fs = fs_with(&["a", "b", "c"]);
        let d = fs.destination_root(Path::new("/d")).unwrap();
        let space = space_on(&fs);
        let mut t = || Ok(());
        let mut idx = NameIndex::for_dir(
            false,
            Some(&d),
            Some(IndexSite { scratch: &space, tick: &mut t }),
            None,
        )
        .unwrap_or_else(|e| panic!("{e:?}"));
        assert!(!idx.is_hybrid());
        for (i, name) in ["n0", "n1"].iter().enumerate() {
            let id = new_file(&fs, name);
            publish(&mut idx, name, None, name, None, id);
            assert_eq!(idx.is_hybrid(), i == 1);
        }
        assert_eq!(probe(&mut idx, &d, "N1"), "entry n1");
        assert_eq!(probe(&mut idx, &d, "A"), "entry a");
    }

    #[test]
    fn a_replacement_applied_to_a_built_table_is_still_wiped_after_migration() {
        let run = |site: bool| {
            let fs = fs_with(&["a.txt", "b.txt"]);
            let id = fs.metadata(Path::new("/d/a.txt")).unwrap().identity;
            fs.set_identity("/d/b.txt", id);
            let x = strong(&fs, "/d/a.txt");
            let d = fs.destination_root(Path::new("/d")).unwrap();
            let space = space_on(&fs);
            let mut t = || Ok(());
            let site = site.then_some(IndexSite { scratch: &space, tick: &mut t });
            let mut idx =
                NameIndex::for_dir(false, Some(&d), site, None).unwrap_or_else(|e| panic!("{e:?}"));
            // Builds the table (the two names are hard links, so this alias is unresolvable).
            assert_eq!(probe(&mut idx, &d, "A.TXT"), "unresolvable");
            let y = replace_object(&fs, "a.txt");
            publish(&mut idx, "t", Some("a.txt"), "a.txt", Some(x), FileIdentity::Strong(y));
            assert!(!idx.is_hybrid());
            for i in 0..3 {
                let name = format!("n{i}");
                let id = new_file(&fs, &name);
                publish(&mut idx, &name, None, &name, None, id);
            }
            (idx.is_hybrid(), probe(&mut idx, &d, "B.TXT"))
        };
        let (inline_hybrid, inline_answer) = run(false);
        let (hybrid, answer) = run(true);
        assert!(!inline_hybrid && hybrid);
        assert_eq!(inline_answer, "unresolvable");
        assert_eq!(answer, "unresolvable");
    }

    #[test]
    fn learned_seqs_keep_their_order_across_the_migration() {
        let run = |site: bool| {
            let fs = fs_with(&["s1", "s2", "s3"]);
            let id = fs.metadata(Path::new("/d/s1")).unwrap().identity;
            fs.set_identity("/d/s2", id);
            fs.set_identity("/d/s3", id);
            let FileIdentity::Strong(x) = id else { panic!("not strong") };
            let d = fs.destination_root(Path::new("/d")).unwrap();
            let space = space_on(&fs);
            let mut t = || Ok(());
            let site = site.then_some(IndexSite { scratch: &space, tick: &mut t });
            let mut idx =
                NameIndex::for_dir(false, Some(&d), site, None).unwrap_or_else(|e| panic!("{e:?}"));
            // Two spellings learned inline: s3 first, then s2.
            publish(&mut idx, "t", Some("s3"), "s3", None, FileIdentity::Strong(x));
            publish(&mut idx, "t", Some("s2"), "s2", None, FileIdentity::Strong(x));
            for i in 0..2 {
                let name = format!("n{i}");
                let id = new_file(&fs, &name);
                publish(&mut idx, &name, None, &name, None, id);
            }
            // A third, learned after the migration.
            publish(&mut idx, "t", Some("s1"), "s1", None, FileIdentity::Strong(x));
            (idx.is_hybrid(), probe(&mut idx, &d, "S1"))
        };
        let (inline_hybrid, inline_answer) = run(false);
        let (hybrid, answer) = run(true);
        assert!(!inline_hybrid && hybrid);
        assert_eq!(inline_answer, "entry s3");
        assert_eq!(answer, "entry s3");
    }

    // Spec 6.5.

    #[test]
    fn more_than_max_candidates_sharing_a_d_ino_makes_no_candidate_call() {
        let fs = fs_with(&["pq.txt", "r1", "r2"]);
        big_dir(&fs, 3);
        fs.set_identity(
            "/d/pq.txt",
            FileIdentity::Strong(ObjectId {
                volume: strong(&fs, "/d/pq.txt").volume,
                index: 7_000,
            }),
        );
        fs.set_ino_mode(Path::new("/d"), crate::fault_fs::InoMode::Shared(7_000));
        let d = fs.destination_root(Path::new("/d")).unwrap();
        let space = space_on(&fs);
        let mut idx = hybrid_over(&space, &d, None);
        let before = count_metadata(&fs);
        assert_eq!(probe(&mut idx, &d, "PQ.TXT"), "entry pq.txt");
        // The probe's own stat plus one stat per name of the identity pass: no candidate was verified.
        assert_eq!(count_metadata(&fs) - before, 1 + 6);
        assert!(idx.identity_built());
    }

    #[test]
    fn a_non_strong_directory_is_keyed_by_an_ordinal_and_a_strong_sibling_does_not_see_its_rows() {
        let fs = fs_with(&["a"]);
        let space = space_on(&fs);
        let strong_dir = ObjectId { volume: 1, index: 99 };
        let mut ordinal = NameIndex::for_new_dir_with(Some(&space), None);
        let mut keyed = NameIndex::for_new_dir_with(Some(&space), Some(strong_dir));
        for (idx, t) in [(&mut ordinal, "t1"), (&mut keyed, "t2")] {
            publish(idx, t, None, "x", None, FileIdentity::Unavailable);
            for i in 0..4 {
                let name = format!("n{i}");
                publish(idx, t, None, &name, None, FileIdentity::Unavailable);
            }
            assert!(idx.is_hybrid());
        }
        assert_eq!(ordinal.owner(os("x")).unwrap(), Some(key("t1")));
        assert_eq!(keyed.owner(os("x")).unwrap(), Some(key("t2")));
        ordinal.retire(&mut || Ok(())).unwrap();
        assert_eq!(keyed.owner(os("x")).unwrap(), Some(key("t2")));
        assert_eq!(keyed.owner(os("n3")).unwrap(), Some(key("t2")));
        assert!(space.store().unwrap().has_name(&DirKey::Strong(strong_dir), os("x")).unwrap());
        assert!(!space.store().unwrap().has_name(&DirKey::Ordinal(1), os("x")).unwrap());
    }

    #[test]
    fn a_popped_child_leaves_the_parents_families_readable() {
        let fs = fs_with(&["p0", "p1", "p2", "p3", "p4", "p5"]);
        fs.create_dir(Path::new("/e")).unwrap();
        for i in 0..6 {
            fs.write_file(format!("/e/c{i}"), b"x");
        }
        let space = space_on(&fs);
        let parent_dir = fs.destination_root(Path::new("/d")).unwrap();
        let child_dir = fs.destination_root(Path::new("/e")).unwrap();
        let mut parent = hybrid_over(&space, &parent_dir, None);
        let parent_end = space.len(Lane::Names);
        assert!(parent_end > 0);
        let child = hybrid_over(&space, &child_dir, None);
        assert!(space.len(Lane::Names) > parent_end);
        child.retire(&mut || Ok(())).unwrap();
        assert_eq!(space.len(Lane::Names), parent_end);
        for i in 0..6 {
            let name = format!("p{i}");
            assert_eq!(probe(&mut parent, &parent_dir, &name), format!("entry {name}"));
        }
    }

    // Failure semantics.

    #[test]
    fn a_store_failure_at_a_commit_boundary_poisons_and_the_next_resolve_fails() {
        faults::arm_write_failure(1);
        let fs = fs_with(&["a"]);
        big_dir(&fs, 5);
        let d = fs.destination_root(Path::new("/d")).unwrap();
        let space = space_on(&fs);
        let mut idx = hybrid_over(&space, &d, None);
        publish(&mut idx, "t", None, "x", None, FileIdentity::Unavailable);
        publish(&mut idx, "t", None, "y", None, FileIdentity::Unavailable);
        assert!(idx.is_poisoned());
        let e = idx.resolve(&d, os("x"), &mut || Ok(())).unwrap_err();
        assert!(e.source.to_string().contains("scratch space is unusable"), "{e}");
        assert!(idx.owner(os("x")).is_err());
    }

    #[test]
    fn a_tick_error_during_the_identity_pass_is_returned() {
        let fs = fs_with(&["FILE.TXT"]);
        big_dir(&fs, 5);
        fs.set_ino_mode(Path::new("/d"), crate::fault_fs::InoMode::Absent);
        let d = fs.destination_root(Path::new("/d")).unwrap();
        let space = space_on(&fs);
        let mut idx = hybrid_over(&space, &d, None);
        let mut failing =
            || Err(flux_fs::FsError::new(Code::IoError, std::io::Error::other("tick says stop")));
        let e = idx.resolve(&d, os("file.txt"), &mut failing).unwrap_err();
        assert!(e.source.to_string().contains("tick says stop"), "{e}");
        assert!(!idx.identity_built());
        assert_eq!(space.len(Lane::Identity), 0);
    }

    #[test]
    fn a_listing_error_during_the_build_is_a_listing_error_and_leaves_the_lanes_empty() {
        let fs = fs_with(&["a"]);
        big_dir(&fs, 7);
        let d = fs.destination_root(Path::new("/d")).unwrap();
        let space = space_on(&fs);
        fs.fail_nth("read_dir", 1, Code::IoError, std::io::ErrorKind::PermissionDenied);
        let mut t = || Ok(());
        let built = NameIndex::for_dir(
            false,
            Some(&d),
            Some(IndexSite { scratch: &space, tick: &mut t }),
            None,
        );
        assert!(matches!(built.err(), Some(BuildError::Listing(_))));
        for lane in [Lane::Names, Lane::Inodes, Lane::Identity] {
            assert_eq!(space.len(lane), 0);
        }
    }

    #[test]
    fn a_tick_error_during_the_build_is_fatal() {
        let fs = fs_with(&["a"]);
        big_dir(&fs, 7);
        let d = fs.destination_root(Path::new("/d")).unwrap();
        let space = space_on(&fs);
        let mut t =
            || Err(flux_fs::FsError::new(Code::IoError, std::io::Error::other("tick says stop")));
        let built = NameIndex::for_dir(
            false,
            Some(&d),
            Some(IndexSite { scratch: &space, tick: &mut t }),
            None,
        );
        let Some(BuildError::Fatal(e)) = built.err() else { panic!("expected a fatal error") };
        assert!(e.source.to_string().contains("tick says stop"), "{e}");
        for lane in [Lane::Names, Lane::Inodes, Lane::Identity] {
            assert_eq!(space.len(lane), 0);
        }
    }

    #[test]
    fn retiring_a_hybrid_index_truncates_its_lanes_and_removes_its_rows() {
        let fs = fs_with(&["FILE.TXT"]);
        big_dir(&fs, 5);
        fs.set_ino_mode(Path::new("/d"), crate::fault_fs::InoMode::Wrong);
        let d = fs.destination_root(Path::new("/d")).unwrap();
        let space = space_on(&fs);
        let dir_id = ObjectId { volume: 1, index: 5 };
        let mut idx = hybrid_over(&space, &d, Some(dir_id));
        publish(&mut idx, "t", None, "x", None, FileIdentity::Unavailable);
        assert_eq!(probe(&mut idx, &d, "file.txt"), "entry FILE.TXT");
        assert!(idx.identity_built());
        for lane in [Lane::Names, Lane::Inodes, Lane::Identity] {
            assert!(space.len(lane) > 0, "{lane:?}");
        }
        assert!(space.store().unwrap().has_name(&DirKey::Strong(dir_id), os("x")).unwrap());
        idx.retire(&mut || Ok(())).unwrap();
        for lane in [Lane::Names, Lane::Inodes, Lane::Identity] {
            assert_eq!(space.len(lane), 0, "{lane:?}");
        }
        assert!(!space.store().unwrap().has_name(&DirKey::Strong(dir_id), os("x")).unwrap());
    }

    // Order.

    #[test]
    fn tier_2_is_consulted_before_the_names_family() {
        let fs = fs_with(&["a"]);
        big_dir(&fs, 5);
        let d = fs.destination_root(Path::new("/d")).unwrap();
        let space = space_on(&fs);
        let mut idx = hybrid_over(&space, &d, None);
        let id = new_file(&fs, "x");
        publish(&mut idx, "t", None, "x", None, id);
        fs.remove_file(Path::new("/d/x")).unwrap();
        // The listing-hit path is taken (the name is known), so the vanished entry is the outer `NotFound`.
        assert_eq!(probe(&mut idx, &d, "x"), "err NotFound");
    }
}
