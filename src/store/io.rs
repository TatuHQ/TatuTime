use std::path::{Path, PathBuf};

use super::{Entity, Store, StoreError};

impl Store {
    /// Generic JSON document read at a root-relative path (auxiliary state
    /// files like the accounting sync status, #33). `Ok(None)` when absent.
    pub fn read_json_rel<T: serde::de::DeserializeOwned>(
        &self,
        rel: &str,
    ) -> Result<Option<T>, StoreError> {
        read_json(&self.root.join(rel))
    }

    /// One-locked read-modify-write of a root-relative JSON document:
    /// `update` sees the current bytes and returns the replacement. Used for
    /// small ledgers where list-then-put across two locks would race (review
    /// D3/B4 — accounting sync records).
    pub fn update_json_rel<T, F>(&self, rel: &str, update: F) -> Result<T, StoreError>
    where
        T: serde::Serialize + serde::de::DeserializeOwned,
        F: FnOnce(Option<T>) -> T,
    {
        let _guard = self.write_lock()?;
        let path = self.root.join(rel);
        let current = read_json::<T>(&path)?;
        let next = update(current);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        write_json(&path, &next)?;
        Ok(next)
    }

    /// Generic atomic JSON write at a root-relative path, under the write lock.
    pub fn write_json_rel<T: serde::Serialize + ?Sized>(
        &self,
        rel: &str,
        value: &T,
    ) -> Result<(), StoreError> {
        let _guard = self.write_lock()?;
        let path = self.root.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        write_json(&path, value)
    }

    // ------------------------------------------------ collection primitives --

    pub(super) fn doc_dir<T: Entity>(&self) -> PathBuf {
        self.root.join(T::DIR)
    }

    pub(super) fn doc_path<T: Entity>(&self, id: &str) -> PathBuf {
        self.doc_dir::<T>().join(format!("{id}.json"))
    }

    /// Read every document in a collection directory. A missing directory is
    /// an empty collection (it is created on first write); dot files and
    /// stranded `.tmp` files are not documents (`dir_entries` skips them).
    /// The caller owns the ordering.
    pub(crate) fn list_docs<T: Entity>(&self) -> Result<Vec<T>, StoreError> {
        self.walk_docs(&self.doc_dir::<T>(), read_json::<T>)
    }

    /// The shared `dir_entries -> json filter -> decode -> collect` walk,
    /// with an injectable decoder: nested collections (projects) plug their
    /// own legacy-aware decode hook here (`domain::project_from_bytes`).
    pub(crate) fn walk_docs<T>(
        &self,
        dir: &Path,
        decode: impl Fn(&Path) -> Result<Option<T>, StoreError>,
    ) -> Result<Vec<T>, StoreError> {
        let mut out = Vec::new();
        if !dir_exists(dir)? {
            return Ok(out);
        }
        for path in dir_entries(dir)? {
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            if let Some(v) = decode(&path)? {
                out.push(v);
            }
        }
        Ok(out)
    }

    pub(crate) fn get_doc<T: Entity>(&self, id: &str) -> Result<Option<T>, StoreError> {
        read_json(&self.doc_path::<T>(id))
    }

    /// Lock, enforce the collection cap, ensure the directory, atomic write —
    /// the one put path for flat collections.
    ///
    /// QA-05: the `MAX_DOCS` cap is a **write-time** guard. It is checked
    /// before a *new* document is persisted (an overwrite can never grow the
    /// collection), under the same writer lock as the write itself, so two
    /// concurrent creates cannot both slip past it.
    pub(crate) fn put_doc<T: Entity>(&self, value: &T) -> Result<(), StoreError> {
        let _guard = self.write_lock()?;
        let path = self.doc_path::<T>(&value.id());
        if !path.exists() {
            self.ensure_capacity::<T>()?;
        }
        std::fs::create_dir_all(self.doc_dir::<T>())?;
        write_json(&path, value)
    }

    /// Refuse a *new* document when its collection already holds `MAX_DOCS`
    /// documents (QA-05). Callers hold the writer lock and have already
    /// established that the document is new, so the count and the write are
    /// one unit. Reads never consult this: an over-cap directory is still
    /// listed in full, so a pre-existing one stays recoverable.
    pub(super) fn ensure_capacity<T: Entity>(&self) -> Result<(), StoreError> {
        if doc_count(&self.doc_dir::<T>())? >= self.max_docs {
            return Err(StoreError::TooManyItems);
        }
        Ok(())
    }

    /// Lock, exists → `NotFound`, remove — the one delete path. Guards that
    /// need to inspect other collections take `write_lock` themselves and
    /// reuse `remove_doc_locked`.
    pub(crate) fn remove_doc<T: Entity>(&self, id: &str) -> Result<(), StoreError> {
        let _guard = self.write_lock()?;
        self.remove_doc_locked::<T>(id)
    }

    pub(crate) fn remove_doc_locked<T: Entity>(&self, id: &str) -> Result<(), StoreError> {
        let path = self.doc_path::<T>(id);
        if !path.exists() {
            return Err(StoreError::NotFound);
        }
        std::fs::remove_file(&path)?;
        Ok(())
    }
}

/// Directory modification time, or `None` when the directory is absent (a
/// missing collection is empty; creating it later changes the stamp).
pub(super) fn dir_mtime(dir: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(dir).and_then(|m| m.modified()).ok()
}

pub(super) fn prune_tmp_files(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.filter_map(|e| e.ok()) {
        let p = e.path();
        if p.file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.ends_with(".tmp"))
            && let Ok(md) = p.metadata()
            && md.is_file()
        {
            let _ = std::fs::remove_file(&p);
        }
    }
}

/// Whether `dir` is a directory: `Ok(true)`/`Ok(false)` when it is present or
/// absent, and an error when something else sits at that path.
///
/// QA-11: `Path::exists()` answers **false** for a path whose parent is a
/// regular file, so a collection directory replaced by a file used to read as
/// an empty collection (200) while every write failed with an opaque 500. A
/// missing directory is legitimately empty (it is created on first write); a
/// non-directory is a broken data tree and must surface as
/// [`StoreError::DataDirInvalid`].
pub(super) fn dir_exists(dir: &Path) -> Result<bool, StoreError> {
    match std::fs::metadata(dir) {
        Ok(meta) if meta.is_dir() => Ok(true),
        Ok(_) => Err(StoreError::DataDirInvalid),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) if e.kind() == std::io::ErrorKind::NotADirectory => Err(StoreError::DataDirInvalid),
        Err(e) => Err(e.into()),
    }
}

/// Read a directory and return its document paths. Sorting by name keeps
/// listings stable across platforms. Dot files (state such as
/// `invoices/.seq.json`) and crashed-write `*.tmp` leftovers (review D6) are
/// **not** documents: they are skipped here and pruned at open.
///
/// QA-05: a read lists **what is there** and never fails because a directory
/// is over the `MAX_DOCS` cap. The cap is enforced on the write path (see
/// [`Store::ensure_capacity`]), so the API can no longer create such a
/// directory; an operator-supplied one stays readable — and therefore
/// recoverable — instead of wedging every listing at 500.
///
/// QA-11: a missing directory is an empty collection; a path that exists as a
/// regular file is refused (`data_dir_invalid`), never reported as empty.
pub(super) fn dir_entries(dir: &Path) -> Result<Vec<PathBuf>, StoreError> {
    if !dir_exists(dir)? {
        return Ok(Vec::new());
    }
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or_default();
            !(name.starts_with('.') || name.ends_with(".tmp"))
        })
        .collect();
    paths.sort();
    Ok(paths)
}

/// How many documents a collection directory holds; `0` when it does not
/// exist yet (it is created on first write). Feeds the write-time cap guard.
pub(super) fn doc_count(dir: &Path) -> Result<usize, StoreError> {
    if !dir_exists(dir)? {
        return Ok(0);
    }
    Ok(dir_entries(dir)?.len())
}

/// The `<id>` stems of every `.json` document among `paths` (a directory
/// listing), ignoring anything else the folder holds.
pub(super) fn json_stems(paths: &[PathBuf]) -> std::collections::BTreeSet<String> {
    paths
        .iter()
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("json"))
        .filter_map(|p| p.file_stem().and_then(|s| s.to_str()))
        .map(str::to_string)
        .collect()
}

pub(super) fn read_json<T>(path: &Path) -> Result<Option<T>, StoreError>
where
    T: serde::de::DeserializeOwned,
{
    match std::fs::read(path) {
        Ok(bytes) => {
            // QA-04/QA-12: a stored document that does not decode is its own
            // *distinguishable* failure (500 `corrupt_document`), with the file
            // and parse error kept for the server log — never for a client.
            let value = serde_json::from_slice(&bytes).map_err(|e| {
                StoreError::CorruptDocument(format!("corrupt document {}: {e}", path.display()))
            })?;
            Ok(Some(value))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Write a document with the store's atomic discipline: pretty JSON into a
/// sibling `.tmp`, then `rename` over the target, so a reader never observes a
/// half-written file. Callers hold the writer lock.
///
/// ```rust
/// write_json(path, &doc)?;   // tmp write, then one atomic rename
/// ```
pub(super) fn write_json<T>(path: &Path, value: &T) -> Result<(), StoreError>
where
    T: serde::Serialize + ?Sized,
{
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|e| StoreError::Io(format!("serialisation failed: {e}")))?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Binary sibling of `write_json` (#113): same atomic tmp + rename discipline
/// under the same writer lock; stranded `.tmp` files are pruned on open.
pub(super) fn write_bytes_atomic(path: &Path, bytes: &[u8]) -> Result<(), StoreError> {
    let tmp = path.with_extension("pdf.tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_json_rel_is_locked_rmw() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("data")).unwrap();
        let out: Vec<u32> = store
            .update_json_rel("counter.json", |cur: Option<Vec<u32>>| {
                let mut v = cur.unwrap_or_default();
                v.push(v.len() as u32 + 1);
                v
            })
            .unwrap();
        assert_eq!(out, vec![1]);
        let out = store
            .update_json_rel("counter.json", |cur: Option<Vec<u32>>| {
                let mut v = cur.unwrap_or_default();
                v.push(v.len() as u32 + 1);
                v
            })
            .unwrap();
        assert_eq!(out, vec![1, 2]);
    }
}
