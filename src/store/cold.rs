use std::collections::HashMap;
use std::fs::File;
use std::path::{Path, PathBuf};

use chrono::{NaiveDate, Utc};
use uuid::Uuid;

use super::io::{dir_entries, write_json};
use super::*;

impl Store {
    // ------------------------------------------------- entries archival (#349) --
    //
    // ADR-003 (`docs/architecture/adr-003-entry-retention-and-archival.md`)
    // defers retention to a measurable trigger and names this primitive its
    // precondition 1: an operator-run pass that relocates whole day folders out
    // of the hot tree (`entries/<YYYY-MM-DD>/`) into the cold tree
    // (`archive/entries/<YYYY-MM>/<YYYY-MM-DD>/`), writes a per-run sha256
    // manifest, drops the relocated days from the #335 per-user index, and
    // refreshes the cold id index (#350). Read-side it changes nothing: every
    // reader merges the cold tree back in (the `cold-tree reads` block above), so
    // a relocated day stays visible to `get_entry`/`list_range`/`list_all_entries`
    // exactly as it was.
    //
    // What may move is not this module's decision (ADR-003: the lock seam and
    // `domain::already_invoiced` remain the sole authority). The `locks`
    // parameter — `CombinedLocks` in production — answers "is this entry
    // frozen", fail-closed on an unverifiable state (#185), and
    // `domain::already_invoiced` answers "has a *non-draft* invoice already
    // billed it" — the permanent exclusion that keeps a settled invoice's
    // rate/hours evidence in the live tree. A day holding either is refused
    // whole and reported: never touched, never half-moved.

    /// Relocate every day folder strictly before `cutoff` whose entries are all
    /// provably unlocked and unbilled (ADR-003 precondition 1, #349). Returns
    /// what the pass did — or, with `dry_run`, what it *would* do while writing
    /// absolutely nothing (no cold tree, no manifest, no index rewrite, no
    /// temporary file left behind).
    ///
    /// The whole pass runs under the one write lock, so a move can never race a
    /// concurrent `put_entry`; reads never take that lock, so serving continues.
    ///
    /// `locks` is the #18 seam, not a predicate this module re-derives. A day is
    /// refused — skipped, reported, left byte-identical — when it holds an entry
    /// frozen by that seam ([`ArchiveRefusal::Locked`]), an entry on a non-draft
    /// invoice ([`ArchiveRefusal::AlreadyInvoiced`]), or when the lock state
    /// cannot be verified at all ([`ArchiveRefusal::LockUnavailable`], fail
    /// closed). A day whose cold copy already exists is refused
    /// ([`ArchiveRefusal::ColdDayExists`]) rather than overwritten, and anything
    /// under `entries/` that is not a well-formed day folder of `<uuid>.json`
    /// documents is refused ([`ArchiveRefusal::Unrecognised`]).
    ///
    /// A relocation is all-or-nothing per day: the cold copy is assembled in a
    /// hidden staging folder and published with one `rename`, and the hot day is
    /// removed last, so a crash can leave a day in both trees but never in
    /// neither and never half-archived. The per-run manifest of sha256s and the
    /// one index rewrite land once the last day has moved; a pass that relocates
    /// nothing writes neither. An interrupted pass therefore leaves the days it
    /// had already published cold but unmanifested and still named by the index —
    /// nothing is lost (the hot day goes only after its cold copy is complete),
    /// and the #335 trust rule heals the index on the next member read.
    ///
    /// A day folder holding no document has nothing that could be frozen or
    /// billed, so it is relocated like any other clean day; both counts are zero
    /// in that case.
    pub fn archive_entries_before(
        &self,
        cutoff: NaiveDate,
        dry_run: bool,
        locks: &dyn crate::lock::EntryLock,
    ) -> Result<ArchiveReport, StoreError> {
        let _guard = self.write_lock()?;
        let mut report = ArchiveReport {
            dry_run,
            cutoff: cutoff.format("%Y-%m-%d").to_string(),
            ..ArchiveReport::default()
        };

        // The two refusal sets, resolved once for the whole pass: the #18 seam
        // (fail closed — an unverifiable state refuses every candidate day
        // rather than archiving behind its back) and every entry a *non-draft*
        // invoice already billed, through the shared predicate (#248).
        let locked = locks.locked_entries();
        if let Err(unavailable) = &locked {
            tracing::warn!(
                error = %unavailable.0,
                "archive_entries_before: lock state unverifiable; refusing every candidate day"
            );
        }
        let billed: std::collections::HashSet<Uuid> =
            crate::domain::already_invoiced(&self.list_invoices()?)
                .0
                .into_iter()
                .collect();

        // Candidates: the immediate children of `entries/` that are day folders
        // older than the cutoff. Anything else the folder holds is reported as
        // unrecognised — a relocation has to prove it understood the day before
        // it touches it.
        let entries_dir = self.root.join(ENTRIES_DIR);
        let mut candidates: Vec<NaiveDate> = Vec::new();
        if entries_dir.exists() {
            for child in dir_entries(&entries_dir, self.max_docs)? {
                let name = child
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or_default()
                    .to_string();
                match NaiveDate::parse_from_str(&name, "%Y-%m-%d") {
                    // Out of scope (on or after the cutoff): neither a candidate
                    // nor a refusal — the pass leaves it exactly as it found it.
                    Ok(date) if child.is_dir() && date >= cutoff => {}
                    Ok(date) if child.is_dir() => candidates.push(date),
                    // A stray document, a directory that is not a day name, or a
                    // name that is not a date at all: reported, never touched.
                    _ => report.skipped.push(SkippedDay {
                        day: name,
                        entries: 0,
                        reason: ArchiveRefusal::Unrecognised,
                    }),
                }
            }
        }
        candidates.sort();

        let mut manifest: Vec<crate::backup::FileEntry> = Vec::new();
        let mut relocated: Vec<NaiveDate> = Vec::new();
        for date in candidates {
            let day = self.day_dir(date);
            // The day's documents by file name alone: an entry's id *is* its
            // document stem, so neither the refusal check nor the manifest ever
            // decodes the money/hours a relocation must not touch.
            let docs = dir_entries(&day, self.max_docs)?;
            let mut ids: Vec<Uuid> = Vec::with_capacity(docs.len());
            let mut unrecognised = false;
            for doc in &docs {
                match doc
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .and_then(|s| Uuid::parse_str(s).ok())
                {
                    Some(id) if doc.is_file() => ids.push(id),
                    _ => unrecognised = true,
                }
            }
            let refusal = if unrecognised {
                Some(ArchiveRefusal::Unrecognised)
            } else {
                match &locked {
                    // A day is never archived behind an inconclusive check.
                    Err(_) => Some(ArchiveRefusal::LockUnavailable),
                    Ok(frozen) if ids.iter().any(|id| frozen.contains(id)) => {
                        Some(ArchiveRefusal::Locked)
                    }
                    Ok(_) if ids.iter().any(|id| billed.contains(id)) => {
                        Some(ArchiveRefusal::AlreadyInvoiced)
                    }
                    // Already cold: fail closed rather than merge or overwrite.
                    Ok(_) if self.cold_day_dir(date).exists() => {
                        Some(ArchiveRefusal::ColdDayExists)
                    }
                    Ok(_) => None,
                }
            };
            if let Some(reason) = refusal {
                report.skipped_entries += docs.len() as u64;
                report.skipped.push(SkippedDay {
                    day: date.format("%Y-%m-%d").to_string(),
                    entries: docs.len() as u64,
                    reason,
                });
                continue;
            }
            let mut bytes = 0u64;
            for doc in &docs {
                bytes += std::fs::metadata(doc)?.len();
            }
            let row = ArchivedDay {
                day: date.format("%Y-%m-%d").to_string(),
                documents: docs.len() as u64,
                bytes,
            };
            report.documents += row.documents;
            report.bytes += row.bytes;
            if !dry_run {
                manifest.extend(self.relocate_day_locked(date, &day, &docs)?);
                relocated.push(date);
            }
            report.days.push(row);
        }

        if !relocated.is_empty() {
            // The index may only name days that are on disk (#335): drop the
            // relocated days' keys in the same pass, or it keeps a slice for a
            // folder that is gone.
            self.drop_archived_day_keys_locked(&relocated)?;
            // #350: the cold id index is written here and nowhere else, rebuilt
            // from the whole cold tree; read-side it is a hint, so a read that
            // finds it stale or missing walks the tree and is still correct.
            self.write_cold_index_locked()?;
            let rel = self.archive_manifest_rel();
            self.write_archive_manifest_locked(&rel, cutoff, &manifest)?;
            report.manifest = Some(rel);
        }
        report.skipped.sort_by_key(|row| row.day.clone());
        Ok(report)
    }

    /// Rebuild `archive/entries/.idx.ids.json` — `{ "<YYYY-MM-DD>": ["<id>", …] }`
    /// — from the published cold tree (#350). The pass calls it once, in the same
    /// end-of-run block that prunes the per-user index and writes the manifest; no
    /// read ever writes it, and no read trusts a slice without checking it against
    /// the cold day folder it describes.
    ///
    /// The index is derived from the tree rather than appended to, so it always
    /// describes the days *every* pass has published. A rehydrated entry (#350)
    /// leaves its row stale until the next pass, which the per-day check turns
    /// into a walk rather than a wrong answer.
    fn write_cold_index_locked(&self) -> Result<(), StoreError> {
        let mut idx = ColdIdIndex::new();
        for date in self.cold_day_folders()? {
            let ids = self.cold_day_ids(date)?;
            if !ids.is_empty() {
                idx.insert(
                    date.format("%Y-%m-%d").to_string(),
                    ids.into_iter().collect(),
                );
            }
        }
        let path = self.cold_index_path();
        let Some(parent) = path.parent() else {
            return Err(StoreError::Io("cold index path has no parent".into()));
        };
        std::fs::create_dir_all(parent)?;
        write_json(&path, &idx)
    }

    /// Publish one day into the cold tree byte for byte and return its manifest
    /// rows. The caller holds the write lock and has already refused the day if
    /// it is frozen, billed, already cold or not recognisably a day.
    ///
    /// The cold copy is assembled in a hidden `.staging` folder that is `rename`d
    /// into place as its last step, so the archive never shows a partial day, and
    /// the hot day is removed only once its cold copy is complete — a crash can
    /// duplicate a day, never lose one. Each document is copied with the store's
    /// tmp+rename discipline; nothing is decoded, so the money/hours invariants
    /// (minor units, hundredths) cannot be touched by a relocation.
    fn relocate_day_locked(
        &self,
        date: NaiveDate,
        day: &Path,
        docs: &[PathBuf],
    ) -> Result<Vec<crate::backup::FileEntry>, StoreError> {
        let cold = self.cold_day_dir(date);
        let month = cold
            .parent()
            .ok_or_else(|| StoreError::Io("archive day path has no parent".into()))?;
        std::fs::create_dir_all(month)?;
        let name = date.format("%Y-%m-%d").to_string();
        let staging = month.join(format!(".{name}.staging"));
        // A staging folder can only be a crash leftover of a pass whose hot day is
        // still here (the hot day goes last), so its content is a duplicate of
        // bytes about to be copied again.
        if staging.exists() {
            std::fs::remove_dir_all(&staging)?;
        }
        std::fs::create_dir_all(&staging)?;
        let mut rows = Vec::with_capacity(docs.len());
        for doc in docs {
            let file = doc
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or_else(|| StoreError::Io("entry document has no file name".into()))?;
            let (size, sha256) = copy_document_atomic(doc, &staging.join(file))?;
            rows.push(crate::backup::FileEntry {
                path: format!(
                    "{ARCHIVE_DIR}/{ENTRIES_DIR}/{}/{name}/{file}",
                    date.format("%Y-%m")
                ),
                size,
                sha256,
            });
        }
        std::fs::rename(&staging, &cold)?;
        std::fs::remove_dir_all(day)?;
        Ok(rows)
    }

    /// Drop the relocated days from the #335 per-user index, so it never names a
    /// day absent from disk (the index's own trust rule compares exactly that,
    /// #335). The caller holds the write lock; a missing or unreadable index is
    /// "no index", so nothing is rewritten.
    fn drop_archived_day_keys_locked(&self, days: &[NaiveDate]) -> Result<(), StoreError> {
        let Some(mut idx) = self.read_entry_index() else {
            return Ok(());
        };
        let keys: Vec<String> = days
            .iter()
            .map(|d| d.format("%Y-%m-%d").to_string())
            .collect();
        let before = idx.days.len();
        for key in &keys {
            idx.days.remove(key);
        }
        if idx.days.len() == before {
            return Ok(());
        }
        write_json(&self.entry_index_path(), &idx)
    }

    /// The data-root-relative path of this run's manifest:
    /// `archive/entries/.manifest.<UTC run stamp>-<random>.json`. One file per
    /// run — the ADR keeps the manifest with the operator's backup — and unique,
    /// so two runs can never overwrite each other's record.
    fn archive_manifest_rel(&self) -> String {
        let stamp = Utc::now().format("%Y%m%dT%H%M%SZ");
        let unique: String = Uuid::new_v4()
            .simple()
            .to_string()
            .chars()
            .take(8)
            .collect();
        format!("{ARCHIVE_DIR}/{ENTRIES_DIR}/.manifest.{stamp}-{unique}.json")
    }

    /// Write the run's manifest: one [`crate::backup::FileEntry`] row (path, size,
    /// sha256) per document relocated, in the shape `--backup` and the #113 PDF
    /// archive already prove a file with, so a restore or an operator can verify
    /// the cold tree byte for byte. Atomic like every other write; the caller
    /// holds the lock.
    fn write_archive_manifest_locked(
        &self,
        rel: &str,
        cutoff: NaiveDate,
        files: &[crate::backup::FileEntry],
    ) -> Result<(), StoreError> {
        let cutoff = cutoff.format("%Y-%m-%d").to_string();
        write_json(
            &self.root.join(rel),
            &ArchiveManifest {
                app: "tucano-time",
                version: env!("CARGO_PKG_VERSION"),
                created_at: Utc::now().to_rfc3339(),
                cutoff,
                files,
            },
        )
    }
}

// -------------------------- entries archival report and manifest types (#349) --

/// What one [`Store::archive_entries_before`] pass did, or — for a dry run — what
/// it would have done without writing anything (#349). The operator-facing report
/// the ADR's trigger asks for: days, documents, bytes and the entries skipped, so
/// a `--dry-run` can be read before the real pass and the manifest kept with the
/// backup afterwards.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub struct ArchiveReport {
    /// Whether this was a dry run (nothing was written).
    pub dry_run: bool,
    /// The cutoff the pass was given (`YYYY-MM-DD`): days strictly before it are
    /// candidates, a day on or after it is never touched and never reported.
    pub cutoff: String,
    /// Day folders relocated (a dry run: that would be relocated), ascending.
    pub days: Vec<ArchivedDay>,
    /// Day folders refused, with the reason and how many documents they hold.
    pub skipped: Vec<SkippedDay>,
    /// Documents relocated in total (a dry run: that would be).
    pub documents: u64,
    /// Bytes relocated in total (a dry run: that would be).
    pub bytes: u64,
    /// Documents left in the hot tree because their day was refused.
    pub skipped_entries: u64,
    /// Data-root-relative path of the sha256 manifest a real pass wrote; `None` on
    /// every dry run and whenever the pass relocated nothing.
    pub manifest: Option<String>,
}

/// One day folder a pass relocated — or, in a dry run, would relocate.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ArchivedDay {
    /// The day, `YYYY-MM-DD`.
    pub day: String,
    /// Documents relocated with it.
    pub documents: u64,
    /// Their total size in bytes.
    pub bytes: u64,
}

/// One day folder a pass refused to relocate.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct SkippedDay {
    /// The day folder's name: `YYYY-MM-DD`, or whatever the directory is called
    /// when it is not a day folder at all.
    pub day: String,
    /// Documents on it.
    pub entries: u64,
    /// Why it stayed in the hot tree.
    pub reason: ArchiveRefusal,
}

/// Why a day folder stayed in the hot tree (#349).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ArchiveRefusal {
    /// An entry on the day is frozen by the #18 seam (`CombinedLocks`).
    Locked,
    /// An entry on the day is on a non-draft invoice (`domain::already_invoiced`):
    /// the permanent refusal, because a settled or written-off invoice's line is
    /// still the evidence for a number that was issued (ADR-003).
    AlreadyInvoiced,
    /// The lock state could not be verified, so the day is refused fail-closed
    /// (#185) rather than archived behind the seam's back.
    LockUnavailable,
    /// The cold day folder is already there: the archive is never overwritten, so
    /// an interrupted pass is resolved by the operator, not by a merge.
    ColdDayExists,
    /// The folder is not a well-formed day folder of `<uuid>.json` documents.
    Unrecognised,
}

/// The per-run relocation manifest (#349): `backup::Manifest`'s header plus the
/// cutoff the run was given, over the [`crate::backup::FileEntry`] rows (path,
/// size, sha256) that shape already proves an archived file with (#94/#113).
#[derive(Debug, serde::Serialize)]
struct ArchiveManifest<'a> {
    app: &'a str,
    version: &'a str,
    created_at: String,
    cutoff: String,
    files: &'a [crate::backup::FileEntry],
}

/// Copy one document byte for byte into `dest` with the store's tmp+rename
/// discipline, returning its size and sha256 — the values the manifest records.
/// The bytes are streamed through a 64 KiB buffer and never decoded, so a
/// relocation can neither re-encode money/hours nor blow a memory budget, and the
/// hash covers exactly the bytes the archive received.
fn copy_document_atomic(src: &Path, dest: &Path) -> Result<(u64, String), StoreError> {
    use sha2::Digest;
    use std::io::Read;
    let tmp = dest.with_extension("json.tmp");
    std::fs::copy(src, &tmp)?;
    let mut file = File::open(&tmp)?;
    let mut hasher = sha2::Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let size = file.metadata()?.len();
    drop(file);
    std::fs::rename(&tmp, dest)?;
    Ok((size, crate::hex_lower(&hasher.finalize())))
}

/// The cold id index (#350): `date -> ids` over the published cold days, in
/// `archive/entries/.idx.ids.json`. A dot file like the indexes below, so it is
/// never a document and never counts toward `MAX_DOCS`; written only by the
/// archival pass, and validated slice by slice against the cold day folder it
/// describes before any read believes it.
pub(super) type ColdIdIndex = HashMap<String, Vec<String>>;
