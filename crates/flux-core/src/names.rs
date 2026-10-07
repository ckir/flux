//! Destination name resolution (cut 8b): the STORED spelling of an existing entry, by listing and identity.
//!
//! Design authority: `docs/superpowers/specs/2026-10-07-cut-8b-replacement-design.md`, "Name resolution".

use flux_fs::{DirHandle, FileIdentity, FluxPathKey, FsError, Metadata, ObjectId};
use std::collections::{BTreeSet, HashMap};
use std::ffi::{OsStr, OsString};

/// One destination directory's names: its listing, the identity table (built lazily, once) and which target of this
/// run wrote which spelling.
pub(crate) struct NameIndex {
    listing: BTreeSet<OsString>,
    /// Strong identity -> its spellings, in the order they were learned (listing order, then publications). `None`
    /// until the first by-identity resolution.
    table: Option<HashMap<ObjectId, Vec<OsString>>>,
    /// Spelling -> the target of this run that wrote it.
    owners: HashMap<OsString, FluxPathKey>,
    /// Publications recorded while `table` is `None`, replayed over the table when it is built.
    pending: Vec<Pending>,
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

impl NameIndex {
    /// A directory this run created: empty listing; `resolve` is not used for it (decision 6).
    pub(crate) fn for_new_dir() -> Self {
        Self { listing: BTreeSet::new(), table: None, owners: HashMap::new(), pending: Vec::new() }
    }

    /// A pre-existing directory: reads `dir.read_dir()` once.
    pub(crate) fn for_existing<D: DirHandle>(dir: &D) -> flux_fs::Result<Self> {
        let mut index = Self::for_new_dir();
        index.listing = dir.read_dir()?.into_iter().map(|e| e.name).collect();
        Ok(index)
    }

    pub(crate) fn resolve<D: DirHandle>(
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
        let first = &spellings[0];
        if spellings.len() > 1 {
            // Several spellings are ONE entry only when this run wrote every one of them for the same target.
            let owner = self.owners.get(first);
            if owner.is_none() || spellings.iter().any(|s| self.owners.get(s) != owner) {
                return Ok(Err(NameError::Unresolvable));
            }
        }
        Ok(Ok(Resolved::Entry { stored: first.clone(), meta }))
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
        let mut spellings = Vec::with_capacity(2);
        if let Some(s) = stored {
            spellings.push(s.to_os_string());
        }
        if !spellings.iter().any(|s| s == planned) {
            spellings.push(planned.to_os_string());
        }
        for s in &spellings {
            self.listing.insert(s.clone());
            self.owners.insert(s.clone(), target.clone());
        }
        let p = Pending { replaced, published, spellings };
        match &mut self.table {
            Some(table) => Self::apply(table, &p),
            None => self.pending.push(p),
        }
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

    fn key(s: &str) -> FluxPathKey {
        FluxPathKey(s.as_bytes().to_vec())
    }

    fn stored(r: Result<Result<Resolved, NameError>, flux_fs::FsError>) -> OsString {
        match r.unwrap().unwrap() {
            Resolved::Entry { stored, .. } => stored,
            Resolved::Absent => panic!("absent"),
        }
    }

    #[test]
    fn an_exact_name_resolves_to_itself() {
        let fs = fs_with(&["file.txt"]);
        let d = fs.destination_root(Path::new("/d")).unwrap();
        let mut idx = NameIndex::for_existing(&d).unwrap();
        assert_eq!(stored(idx.resolve(&d, OsStr::new("file.txt"))), "file.txt");
    }

    #[test]
    fn an_absent_name_is_absent() {
        let fs = fs_with(&["file.txt"]);
        let d = fs.destination_root(Path::new("/d")).unwrap();
        let mut idx = NameIndex::for_existing(&d).unwrap();
        assert_eq!(idx.resolve(&d, OsStr::new("other.txt")).unwrap(), Ok(Resolved::Absent));
    }

    #[test]
    fn another_spelling_resolves_by_identity_to_the_stored_name() {
        let fs = fs_with(&["FILE.TXT", "other.txt"]);
        let d = fs.destination_root(Path::new("/d")).unwrap();
        let mut idx = NameIndex::for_existing(&d).unwrap();
        assert_eq!(stored(idx.resolve(&d, OsStr::new("file.txt"))), "FILE.TXT");
    }

    #[test]
    fn the_table_is_built_once_per_directory() {
        let fs = fs_with(&["FILE.TXT", "OTHER.TXT"]);
        let d = fs.destination_root(Path::new("/d")).unwrap();
        let mut idx = NameIndex::for_existing(&d).unwrap();
        assert_eq!(stored(idx.resolve(&d, OsStr::new("file.txt"))), "FILE.TXT");
        assert_eq!(stored(idx.resolve(&d, OsStr::new("other.txt"))), "OTHER.TXT");
        let count = |p: &str| fs.calls().iter().filter(|c| c.as_str() == p).count();
        // Each listing entry is stat-ed once for the table; the planned names are stat-ed once per resolution
        // (the case-insensitive fake logs the normalized stored spelling for both).
        assert_eq!(count("metadata(/d/FILE.TXT)"), 2); // table + planned `file.txt`
        assert_eq!(count("metadata(/d/OTHER.TXT)"), 2); // table + planned `other.txt`
        assert_eq!(fs.calls().iter().filter(|c| c.starts_with("read_dir(")).count(), 1);
    }

    #[test]
    fn a_hardlink_alias_in_the_listing_is_unresolvable() {
        let fs = fs_with(&["a.txt", "b.txt"]);
        let id = fs.metadata(Path::new("/d/a.txt")).unwrap().identity;
        fs.set_identity("/d/b.txt", id);
        let d = fs.destination_root(Path::new("/d")).unwrap();
        let mut idx = NameIndex::for_existing(&d).unwrap();
        assert_eq!(idx.resolve(&d, OsStr::new("A.TXT")).unwrap(), Err(NameError::Unresolvable));
    }

    #[test]
    fn a_weak_identity_is_unresolvable() {
        let fs = fs_with(&["FILE.TXT"]);
        let id = strong(&fs, "/d/FILE.TXT");
        fs.set_identity("/d/FILE.TXT", FileIdentity::Weak(id));
        let d = fs.destination_root(Path::new("/d")).unwrap();
        let mut idx = NameIndex::for_existing(&d).unwrap();
        assert_eq!(idx.resolve(&d, OsStr::new("file.txt")).unwrap(), Err(NameError::Unresolvable));
        fs.set_identity("/d/FILE.TXT", FileIdentity::Unavailable);
        assert_eq!(idx.resolve(&d, OsStr::new("file.txt")).unwrap(), Err(NameError::Unresolvable));
    }

    #[test]
    fn after_a_replace_the_new_object_resolves_through_both_spellings() {
        let fs = fs_with(&["FILE.TXT"]);
        let old = strong(&fs, "/d/FILE.TXT");
        let d = fs.destination_root(Path::new("/d")).unwrap();
        let mut idx = NameIndex::for_existing(&d).unwrap();
        // Build the table before the publication, so the update path (not the lazy build) is what is measured.
        assert_eq!(stored(idx.resolve(&d, OsStr::new("file.txt"))), "FILE.TXT");
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
        assert_eq!(stored(idx.resolve(&d, OsStr::new("fIlE.tXt"))), "FILE.TXT");
        // The replaced object's identity is gone from the table.
        assert!(!idx.table.as_ref().unwrap().contains_key(&old));
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
        assert_eq!(idx.resolve(&d, OsStr::new("fILE.txt")).unwrap(), Err(NameError::Unresolvable));
    }

    #[test]
    fn a_new_dirs_index_has_an_empty_listing_and_no_reads() {
        let fs = fs_with(&[]);
        let idx = NameIndex::for_new_dir();
        assert!(idx.listing.is_empty());
        assert!(fs.calls().iter().all(|c| !c.starts_with("read_dir(")));
    }

    #[test]
    fn a_metadata_error_other_than_not_found_is_returned_as_the_outer_error() {
        let fs = fs_with(&["FILE.TXT"]);
        let d = fs.destination_root(Path::new("/d")).unwrap();
        let mut idx = NameIndex::for_existing(&d).unwrap();
        fs.fail_kind("metadata", Code::IoError, std::io::ErrorKind::PermissionDenied);
        let e = idx.resolve(&d, OsStr::new("file.txt")).unwrap_err();
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
        assert_eq!(idx.resolve(&d, OsStr::new("file.txt")).unwrap(), Err(NameError::Unresolvable));
        // Any other error while building is a real fault and propagates.
        let fs = fs_with(&["FILE.TXT"]);
        let d = fs.destination_root(Path::new("/d")).unwrap();
        let mut idx = NameIndex::for_existing(&d).unwrap();
        fs.fail_nth("metadata", 2, Code::IoError, std::io::ErrorKind::PermissionDenied);
        let e = idx.resolve(&d, OsStr::new("file.txt")).unwrap_err();
        assert_eq!(e.source.kind(), std::io::ErrorKind::PermissionDenied);
        // And a failed build is not remembered: the next call retries and succeeds.
        assert_eq!(stored(idx.resolve(&d, OsStr::new("file.txt"))), "FILE.TXT");
    }

    #[test]
    fn resolve_does_not_mutate_the_directory() {
        let fs = fs_with(&["FILE.TXT"]);
        let d = fs.destination_root(Path::new("/d")).unwrap();
        let mut idx = NameIndex::for_existing(&d).unwrap();
        let before = fs.calls().len(); // the fixture's own create_dir / read_dir are not under test
        idx.resolve(&d, OsStr::new("file.txt")).unwrap().unwrap();
        idx.resolve(&d, OsStr::new("nope")).unwrap().unwrap();
        let calls = fs.calls().split_off(before);
        assert!(!calls.is_empty());
        assert!(calls.iter().all(|c| c.starts_with("metadata(")), "{calls:?}");
        assert_eq!(fs.read_file("/d/FILE.TXT"), Some(b"x".to_vec()));
    }
}
