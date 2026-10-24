//! Backup/restore (#94). `tatu-time --backup out.tar.gz` archives the whole
//! data dir (users, entries, invoices, vault store, config.json, scheduler
//! state — everything that makes an installation *itself*); `--restore` unpacks
//! it back. The archive is a plain `.tar.gz` with a `manifest.json` recording
//! app version, file list and sha256 checksums, so restores verify integrity
//! and operators can inspect with stock tools.
//!
//! The same module also owns the in-app **ZIP** backups behind Settings →
//! Backup (scheduled daily/weekly/monthly or manual): a plain ZIP of the same
//! tree, written to a sibling `Backup/` folder with a timestamped,
//! company-named file. Its files sit at the archive root, so restoring is
//! "extract into the data directory" — no manifest and no encryption (yet).
//!
//! Refusals protect against data loss: the live server's write lock must be
//! free, and restoring over a non-empty data dir needs `--force`. A backup
//! containing `secrets.bin` prints a reminder to supply the original vault key,
//! because without it the Settings tab will be unreadable after restore.

use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Serialize, Deserialize)]
pub struct Manifest {
    pub app: String,
    pub version: String,
    pub created_at: String,
    pub files: Vec<FileEntry>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct FileEntry {
    pub path: String,
    pub size: u64,
    pub sha256: String,
}

#[derive(Debug, thiserror::Error)]
pub enum BackupError {
    #[error("io: {0}")]
    Io(String),
    #[error(
        "the server holds the data-dir write lock — stop it (or scale down) before backup/restore"
    )]
    Locked,
    #[error("destination is not empty; pass --force to overwrite")]
    NotEmpty,
    #[error("archive integrity check failed: {0}")]
    Corrupt(String),
    #[error("unsafe path in archive: {0}")]
    UnsafePath(String),
}

impl From<std::io::Error> for BackupError {
    fn from(e: std::io::Error) -> Self {
        BackupError::Io(e.to_string())
    }
}

/// Collect every regular file under `root` as (relative path, absolute path),
/// sorted and skipping tmp/lock artefacts.
fn walk(root: &Path) -> Result<Vec<(String, PathBuf)>, BackupError> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy().to_string();
            if path.is_dir() {
                // `.restoring` (and any dot-dir) is transient machinery.
                if !name.starts_with('.') {
                    stack.push(path);
                }
                continue;
            }
            // Transient/runtime files are never part of a backup. Dot files
            // like `invoices/.seq.json` ARE kept — the numbering counter must
            // survive a restore too (review B3/D5).
            if name.ends_with(".tmp") || name.ends_with(".lock") {
                continue;
            }
            let rel = path
                .strip_prefix(root)
                .map_err(|_| BackupError::Io("walk escaped root".into()))?
                .to_string_lossy()
                .replace(std::path::MAIN_SEPARATOR, "/");
            out.push((rel, path));
        }
    }
    out.sort();
    Ok(out)
}

fn finalize_hex(h: &Sha256) -> String {
    h.clone()
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Stream-hash a file (no whole-file read).
fn hash_file(path: &Path) -> Result<(u64, String), BackupError> {
    use std::io::Read;
    let mut f = std::fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok((f.metadata()?.len(), finalize_hex(&h)))
}

/// Take the store's lock file without blocking and **hold it** for the
/// duration (returned `File` releases on drop). The server grabs this same
/// lock briefly around every atomic write, so holding it means: refuse if a
/// write is in flight right now, and block any write that starts while we
/// snapshot — a point-in-time, cross-file-consistent backup.
fn acquire_lock(root: &Path) -> Result<std::fs::File, BackupError> {
    use fs2::FileExt;
    let lock = root.join(".tatutime.lock");
    let f = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lock)
        .map_err(|e| BackupError::Io(e.to_string()))?;
    f.try_lock_exclusive().map_err(|_| BackupError::Locked)?;
    Ok(f)
}

/// Create `out.tar.gz` from the data dir. Returns the manifest file count.
pub fn create(root: &Path, out: &Path) -> Result<usize, BackupError> {
    if !root.is_dir() {
        return Err(BackupError::Io(format!(
            "data dir {} does not exist",
            root.display()
        )));
    }
    // Lock or fail: never snapshot through an in-flight write, and hold off
    // new writes for the duration of the copy.
    let _lock = acquire_lock(root)?;
    // Two streaming passes (hash, then copy): the archive is never held in
    // memory (review D5) — the lock guarantees nothing changes in between.
    let files = walk(root)?;
    let mut entries = Vec::new();
    for (rel, abs) in &files {
        let (size, digest) = hash_file(abs)?;
        entries.push(FileEntry {
            path: rel.clone(),
            size,
            sha256: digest,
        });
    }
    let manifest = Manifest {
        app: "tatu-time".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        created_at: chrono::Utc::now().to_rfc3339(),
        files: entries,
    };
    let manifest_json =
        serde_json::to_vec_pretty(&manifest).map_err(|e| BackupError::Io(e.to_string()))?;

    let file = std::fs::File::create(out)?;
    let mut gz = GzEncoder::new(file, Compression::default());
    tar_write(&mut gz, "manifest.json", &manifest_json)?;
    for (rel, abs) in &files {
        let mut f = std::fs::File::open(abs)?;
        let size = f.metadata()?.len();
        tar_write_stream(&mut gz, rel, &mut f, size)?;
    }
    tar_finish(&mut gz)?;
    gz.finish()?;
    Ok(files.len())
}

/// Restore `in.tar.gz` into `target` (the data dir). Returns operator-facing
/// notes (vault reminder etc.).
///
/// Streaming (review D5): the manifest must be the FIRST entry; each body is
/// copied into a staging folder while hashing, verified against the manifest,
/// then moved into place — memory stays O(1) per file.
pub fn restore(in_path: &Path, target: &Path, force: bool) -> Result<Vec<String>, BackupError> {
    if target.exists() && !force && std::fs::read_dir(target)?.next().is_some() {
        return Err(BackupError::NotEmpty);
    }
    // Never unpack under a live server: take the same lock `create` uses.
    let _lock = if target.exists() {
        Some(acquire_lock(target)?)
    } else {
        None
    };
    std::fs::create_dir_all(target)?;
    let staging = target.join(".restoring");
    let _ = std::fs::remove_dir_all(&staging); // stale leftovers from a crash
    std::fs::create_dir_all(&staging)?;

    let src = std::fs::File::open(in_path)?;
    let mut gz = GzDecoder::new(src);
    let mut notes = Vec::new();

    // 1) manifest (first entry)
    let manifest = match tar_read(&mut gz)? {
        Some((name, bytes)) if sanitize(&name)? == "manifest.json" => {
            serde_json::from_slice::<Manifest>(&bytes)
                .map_err(|e| BackupError::Corrupt(e.to_string()))?
        }
        Some(_) => {
            return Err(BackupError::Corrupt("manifest.json must be first".into()));
        }
        None => return Err(BackupError::Corrupt("empty archive".into())),
    };
    let mut seen = std::collections::HashSet::new();
    let mut had_vault = false;

    // 2) stream + verify each body
    let result = (|| -> Result<(), BackupError> {
        while let Some((name, mut copy)) = tar_read_stream(&mut gz)? {
            let rel = sanitize(&name)?;
            let entry = manifest
                .files
                .iter()
                .find(|f| f.path == rel)
                .ok_or_else(|| BackupError::Corrupt(format!("{rel}: not in manifest")))?;
            let dest = staging.join(&rel);
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut f = std::fs::File::create(&dest)?;
            let mut h = Sha256::new();
            std::io::copy(&mut TeeReader(&mut h, &mut copy), &mut f)
                .map_err(|e| BackupError::Io(e.to_string()))?;
            let digest = finalize_hex(&h);
            if digest != entry.sha256 {
                return Err(BackupError::Corrupt(format!(
                    "{rel}: sha256 mismatch (archive is older than manifest?)"
                )));
            }
            if rel == "secrets.bin" {
                had_vault = true;
                notes.push(
                    "backup contains the encrypted vault store (secrets.bin): restore \
                     <data>/vault.key together with the data (or supply the external \
                     TATU_SECRET_KEY / TATU_SECRET_KEY_FILE override) or the stored \
                     credentials cannot be decrypted"
                        .into(),
                );
            }
            seen.insert(rel);
        }
        for entry in &manifest.files {
            if !seen.contains(&entry.path) {
                return Err(BackupError::Corrupt(format!(
                    "{} missing from archive",
                    entry.path
                )));
            }
        }
        Ok(())
    })();
    if let Err(e) = result {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(e);
    }

    // 3) publish verified files
    for entry in &manifest.files {
        let from = staging.join(&entry.path);
        let to = target.join(&entry.path);
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::rename(&from, &to)?;
    }
    let _ = std::fs::remove_dir_all(&staging);

    // Operator nudge: a vault store without its key is unreadable on boot.
    let key_present = std::env::var("TATU_SECRET_KEY")
        .ok()
        .or_else(|| std::env::var("TATU_SECRET_KEY_FILE").ok())
        .is_some_and(|v| !v.is_empty());
    if !key_present && had_vault {
        notes.push(
            "WARNING: no TATU_SECRET_KEY(_FILE) is set in this environment, but the \
             backup carries a vault store."
                .into(),
        );
    }
    Ok(notes)
}

/// One streamed archive entry: name + seekable body cursor.
type ArchiveEntry = (String, std::io::Cursor<Vec<u8>>);

/// Read one tar entry (name + body). The body is held one-file-at-a-time —
/// memory is O(largest file), not O(whole data dir) as before (review D5).
fn tar_read_stream<R: Read>(r: &mut R) -> Result<Option<ArchiveEntry>, BackupError> {
    let mut header = [0u8; 512];
    if !read_exact_or_eof(r, &mut header)? {
        return Ok(None);
    }
    if header.iter().all(|b| *b == 0) {
        return Ok(None);
    }
    let name = cstr(&header[0..100]);
    let size = octal(&header[124..136])? as usize;
    let pad = (512 - (size % 512)) % 512;
    // Caller consumes `size` bytes; we leave body in the stream, and the pad
    // is skipped after the copy. To keep the API honest we wrap the reader:
    // copy exactly size, then consume pad.
    let mut limiter = r.take(size as u64);
    let mut body = Vec::new();
    std::io::Read::read_to_end(&mut limiter, &mut body)
        .map_err(|e| BackupError::Io(e.to_string()))?;
    let mut discard = [0u8; 512];
    let mut left = pad;
    while left > 0 {
        let n = left.min(512);
        r.read_exact(&mut discard[..n])?;
        left -= n;
    }
    Ok(Some((name, std::io::Cursor::new(body))))
}

/// Tee bytes through a hasher while copying.
struct TeeReader<'a, H: sha2::Digest, R>(&'a mut H, &'a mut R);
impl<R: Read, H: sha2::Digest> Read for TeeReader<'_, H, R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.1.read(buf)?;
        self.0.update(&buf[..n]);
        Ok(n)
    }
}

/// Reject absolute paths and any `..` component; keep plain relative paths.
fn sanitize(name: &str) -> Result<String, BackupError> {
    let p = Path::new(name);
    if p.is_absolute() {
        return Err(BackupError::UnsafePath(name.into()));
    }
    for c in p.components() {
        if matches!(
            c,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        ) {
            return Err(BackupError::UnsafePath(name.into()));
        }
    }
    Ok(name.to_string())
}

// --------------------------------------------------------- minimal tar ustar --
// 512-byte headers; regular files only; paths are short by construction (the
// data-dir layout is shallow). Private to this module, round-trip tested.
// ustar field layout: name 0..100, mode 100..108, uid 108..116, gid 116..124,
// size 124..136, mtime 136..148, chksum 148..156, typeflag 156, linkname
// 157..257, magic 257..263 ("ustar\0"), version 263..265 ("00").

fn tar_header<W: Write>(w: &mut W, name: &str, size: u64) -> Result<(), BackupError> {
    if name.len() > 99 {
        return Err(BackupError::Io(format!("path too long for tar: {name}")));
    }
    let mut header = [b' '; 512];
    header[..name.len()].copy_from_slice(name.as_bytes());
    write_octal(&mut header[100..108], 0o644);
    write_octal(&mut header[108..116], 0);
    write_octal(&mut header[116..124], 0);
    write_octal(&mut header[124..136], size);
    write_octal(&mut header[136..148], chrono::Utc::now().timestamp() as u64);
    header[156] = b'0'; // regular file
    header[257..263].copy_from_slice(b"ustar\0");
    header[263..265].copy_from_slice(b"00");
    // The checksum covers the header with its own field read as spaces (b' '
    // is already the fill value).
    let sum: u32 = header.iter().map(|b| *b as u32).sum();
    let cs = format!("{sum:06o}\0 ");
    header[148..156].copy_from_slice(cs.as_bytes());
    w.write_all(&header)?;
    Ok(())
}

fn write_octal(field: &mut [u8], value: u64) {
    let n = field.len();
    // (n-1) digits of zero-padded octal, then a NUL terminator.
    let s = format!("{value:0>width$o}", width = n - 1);
    let bytes = s.as_bytes();
    let take = bytes.len().min(n - 1);
    field[..n - 1 - take].fill(b'0');
    field[n - 1 - take..n - 1].copy_from_slice(&bytes[bytes.len() - take..]);
    field[n - 1] = b'\0';
}

/// Tar entry sourced from a reader — the archive never holds a file body.
fn tar_write_stream<W: Write, R: Read>(
    w: &mut W,
    name: &str,
    r: &mut R,
    size: u64,
) -> Result<(), BackupError> {
    tar_header(w, name, size)?;
    let mut limiter = r.take(size);
    std::io::copy(&mut limiter, w).map_err(|e| BackupError::Io(e.to_string()))?;
    let pad = (512 - (size % 512)) % 512;
    if pad > 0 {
        w.write_all(&vec![0u8; pad as usize])?;
    }
    Ok(())
}

fn tar_write<W: Write>(w: &mut W, name: &str, bytes: &[u8]) -> Result<(), BackupError> {
    tar_header(w, name, bytes.len() as u64)?;
    w.write_all(bytes)?;
    let pad = (512 - (bytes.len() % 512)) % 512;
    if pad > 0 {
        w.write_all(&vec![0u8; pad])?;
    }
    Ok(())
}

fn tar_finish<W: Write>(w: &mut W) -> Result<(), BackupError> {
    w.write_all(&[0u8; 1024])?; // two zero blocks terminate the archive
    Ok(())
}

fn tar_read<R: Read>(r: &mut R) -> Result<Option<(String, Vec<u8>)>, BackupError> {
    let mut header = [0u8; 512];
    if !read_exact_or_eof(r, &mut header)? {
        return Ok(None);
    }
    if header.iter().all(|b| *b == 0) {
        return Ok(None); // end-of-archive block
    }
    let name = cstr(&header[0..100]);
    let size = octal(&header[124..136])? as usize;
    let mut bytes = vec![0u8; size];
    r.read_exact(&mut bytes)?;
    let pad = (512 - (size % 512)) % 512;
    let mut discard = [0u8; 512];
    if pad > 0 {
        r.read_exact(&mut discard[..pad])?;
    }
    Ok(Some((name, bytes)))
}

fn read_exact_or_eof<R: Read>(r: &mut R, buf: &mut [u8]) -> Result<bool, BackupError> {
    let mut filled = 0usize;
    while filled < buf.len() {
        let n = r.read(&mut buf[filled..])?;
        if n == 0 {
            return Ok(filled != 0); // partial header is corruption; none is EOF
        }
        filled += n;
    }
    Ok(true)
}

/// Tar text fields are NUL-terminated *and* space-padded; trim both.
fn cstr(field: &[u8]) -> String {
    let end = field.iter().position(|b| *b == 0).unwrap_or(field.len());
    String::from_utf8_lossy(&field[..end])
        .trim_end_matches(' ')
        .to_string()
}

fn octal(field: &[u8]) -> Result<u64, BackupError> {
    let s = cstr(field);
    let s = s.trim();
    if s.is_empty() {
        return Ok(0);
    }
    u64::from_str_radix(s, 8).map_err(|_| BackupError::Corrupt(format!("bad octal {s:?}")))
}

// ============================================================ ZIP backups ==
//
// The operator CLI above archives to `.tar.gz` with a sha256 manifest. The
// in-app feature (Settings → Backup) writes a plain ZIP instead: every file
// below the data directory, at the archive root, so restoring is literally
// "extract this into the data directory". No encryption (yet) — the archive
// contains `secrets.bin` and `vault.key` exactly like the data directory does.

use std::sync::{Arc, Mutex};

use chrono::{
    DateTime, Datelike, Days, Local, LocalResult, Months, NaiveDate, NaiveDateTime, TimeZone, Utc,
};

use crate::appconfig::AppConfig;
use crate::scheduler::Job;

/// The fixed middle of a backup file name. `list_backups` uses it to recognise
/// the files this feature owns, so unrelated ZIPs in the folder are ignored.
pub const FILE_MARKER: &str = "_tatutime-backup_";

/// Where backups live: a `Backup` folder next to the data directory, which is
/// the project-root `Backup/` for the default `./Data` and the volume-mounted
/// `/Backup` in the hardened image. `TATU_BACKUP_DIR` overrides it.
#[must_use]
pub fn directory(root: &Path) -> PathBuf {
    match root.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.join("Backup"),
        _ => PathBuf::from("Backup"),
    }
}

/// Resolve the backup directory from the operator override, falling back to
/// [`directory`]. An empty override is ignored (the environment seam treats an
/// empty string as unset elsewhere too).
#[must_use]
pub fn configured_directory(root: &Path, env: &dyn Fn(&str) -> Option<String>) -> PathBuf {
    env("TATU_BACKUP_DIR")
        .filter(|value| !value.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| directory(root))
}

/// Lower-case ASCII slug of the company name for the file name, matching
/// `capyabarasolutionsltda` from "Capyabara Solutions Ltda". Non-ASCII and
/// punctuation are dropped; an empty name becomes `tatutime`.
#[must_use]
pub fn company_slug(name: &str) -> String {
    let slug: String = name
        .chars()
        .filter_map(|c| {
            if c.is_ascii_alphanumeric() {
                Some(c.to_ascii_lowercase())
            } else {
                None
            }
        })
        .take(60)
        .collect();
    if slug.is_empty() {
        "tatutime".to_string()
    } else {
        slug
    }
}

/// `<datetime>_tatutime-backup_<company>.zip`, timestamped in the server's
/// local time zone (the same zone the schedule uses). Colons are replaced by
/// hyphens so the name is portable.
#[must_use]
pub fn file_name(now: DateTime<Local>, company: &str) -> String {
    format!(
        "{}_tatutime-backup_{}.zip",
        now.format("%Y-%m-%d_%H-%M-%S"),
        company_slug(company)
    )
}

/// Collect every regular file under `root`, skipping transient artefacts and
/// the backup destination (which may sit inside the data directory when
/// `TATU_BACKUP_DIR` points there).
fn walk_for_zip(root: &Path, skip: &[PathBuf]) -> Result<Vec<(String, PathBuf)>, BackupError> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy().to_string();
            if path.is_dir() {
                if !name.starts_with('.') && !skip.iter().any(|s| path.starts_with(s)) {
                    stack.push(path);
                }
                continue;
            }
            if skip.contains(&path) {
                continue;
            }
            if name.ends_with(".tmp") || name.ends_with(".lock") {
                continue;
            }
            let rel = path
                .strip_prefix(root)
                .map_err(|_| BackupError::Io("walk escaped root".into()))?
                .to_string_lossy()
                .replace(std::path::MAIN_SEPARATOR, "/");
            out.push((rel, path));
        }
    }
    out.sort();
    Ok(out)
}

/// Create a ZIP of the data directory at `out`. Returns the number of files
/// written. The archive holds the files at its root (no `Data/` prefix), so
/// extracting it into the data directory restores an installation.
///
/// The store's write lock is held for the duration, exactly like the CLI
/// snapshot, so the archive is point-in-time consistent.
pub fn create_zip(root: &Path, out: &Path) -> Result<usize, BackupError> {
    if !root.is_dir() {
        return Err(BackupError::Io(format!(
            "data dir {} does not exist",
            root.display()
        )));
    }
    let _lock = acquire_lock(root)?;
    let parent = out.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    let tmp = parent.join(format!(
        ".{}.tmp",
        out.file_name().unwrap_or_default().to_string_lossy()
    ));
    let mut skip = vec![out.to_path_buf(), tmp.clone()];
    // If the destination is inside the data directory, never archive it (a
    // backup must not recursively contain itself or its siblings).
    if parent.starts_with(root) && parent != root {
        skip.push(parent.to_path_buf());
    }
    let files = walk_for_zip(root, &skip)?;

    let result = (|| -> Result<(), BackupError> {
        let file = std::fs::File::create(&tmp)?;
        let mut writer = zip::ZipWriter::new(std::io::BufWriter::new(file));
        for (rel, abs) in &files {
            let mut source = std::fs::File::open(abs)?;
            // Keep key material and the vault store private after extraction;
            // ordinary documents use the usual 0644.
            let mode = if matches!(
                rel.as_str(),
                "secrets.bin" | "vault.key" | "session.key" | ".tatutime.lock"
            ) {
                0o600
            } else {
                0o644
            };
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated)
                .unix_permissions(mode);
            writer
                .start_file(rel.clone(), options)
                .map_err(|e| BackupError::Io(format!("zip entry {rel}: {e}")))?;
            std::io::copy(&mut source, &mut writer)
                .map_err(|e| BackupError::Io(format!("zip body {rel}: {e}")))?;
        }
        writer
            .finish()
            .map_err(|e| BackupError::Io(format!("zip finish: {e}")))?;
        Ok(())
    })();
    if let Err(error) = result {
        let _ = std::fs::remove_file(&tmp);
        return Err(error);
    }
    std::fs::rename(&tmp, out)?;
    Ok(files.len())
}

/// A backup that was just written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatedBackup {
    pub file_name: String,
    pub size_bytes: u64,
    pub created_at: DateTime<Utc>,
}

/// A ZIP found in the backup directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupFile {
    pub file_name: String,
    pub size_bytes: u64,
    pub created_at: DateTime<Utc>,
}

/// Write a new backup into `destination`, choosing a unique name when two runs
/// land in the same second.
pub fn run_in(
    destination: &Path,
    root: &Path,
    company: &str,
    now: DateTime<Utc>,
) -> Result<CreatedBackup, BackupError> {
    std::fs::create_dir_all(destination)?;
    let local = now.with_timezone(&Local);
    let base = file_name(local, company);
    let mut name = base.clone();
    let mut out = destination.join(&name);
    let mut counter = 1u32;
    while out.exists() {
        let stem = base.trim_end_matches(".zip");
        name = format!("{stem}-{counter}.zip");
        out = destination.join(&name);
        counter += 1;
    }
    let files = create_zip(root, &out)?;
    let size_bytes = std::fs::metadata(&out)?.len();
    tracing::info!(files, file = %name, "backup written");
    Ok(CreatedBackup {
        file_name: name,
        size_bytes,
        created_at: now,
    })
}

/// List the backups in `destination`, newest first. A missing directory is
/// empty, not an error (nothing has been backed up yet).
pub fn list_backups_in(destination: &Path) -> Result<Vec<BackupFile>, BackupError> {
    let entries = match std::fs::read_dir(destination) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(BackupError::Io(error.to_string())),
    };
    let mut out = Vec::new();
    for entry in entries {
        let Ok(entry) = entry else { continue };
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.ends_with(".zip") || !name.contains(FILE_MARKER) {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if !metadata.is_file() {
            continue;
        }
        let created_at = metadata
            .modified()
            .map(DateTime::<Utc>::from)
            .unwrap_or_else(|_| Utc::now());
        out.push(BackupFile {
            file_name: name,
            size_bytes: metadata.len(),
            created_at,
        });
    }
    out.sort_by_key(|a| std::cmp::Reverse(a.created_at));
    Ok(out)
}

/// Backup cadence. Weekly runs on Monday and monthly on the 1st; the admin UI
/// says so next to the picker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cadence {
    Daily,
    Weekly,
    Monthly,
}

impl Cadence {
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "daily" => Some(Self::Daily),
            "weekly" => Some(Self::Weekly),
            "monthly" => Some(Self::Monthly),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Daily => "daily",
            Self::Weekly => "weekly",
            Self::Monthly => "monthly",
        }
    }
}

fn at(date: NaiveDate, hour: u32, minute: u32) -> NaiveDateTime {
    date.and_hms_opt(hour, minute, 0)
        .expect("validated 0..=23 and 0..=59")
}

fn first_of_month(date: NaiveDate) -> NaiveDate {
    date.with_day(1).expect("every month has a first day")
}

/// The most recent scheduled occurrence at or before `now`.
#[must_use]
pub fn last_occurrence(
    now: NaiveDateTime,
    cadence: Cadence,
    (hour, minute): (u32, u32),
) -> NaiveDateTime {
    let today = now.date();
    match cadence {
        Cadence::Daily => {
            let today_at = at(today, hour, minute);
            if today_at <= now {
                today_at
            } else {
                at(today - Days::new(1), hour, minute)
            }
        }
        Cadence::Weekly => {
            // Monday of the current ISO week, then step back a week when this
            // week's Monday time has not arrived yet.
            let back = u64::from(today.weekday().num_days_from_monday());
            let monday = today - Days::new(back);
            let monday_at = at(monday, hour, minute);
            if monday_at <= now {
                monday_at
            } else {
                at(monday - Days::new(7), hour, minute)
            }
        }
        Cadence::Monthly => {
            let first = first_of_month(today);
            let first_at = at(first, hour, minute);
            if first_at <= now {
                first_at
            } else {
                let previous = first - Days::new(1);
                at(first_of_month(previous), hour, minute)
            }
        }
    }
}

/// The next scheduled occurrence strictly after `now`.
#[must_use]
pub fn next_occurrence(
    now: NaiveDateTime,
    cadence: Cadence,
    (hour, minute): (u32, u32),
) -> NaiveDateTime {
    let today = now.date();
    match cadence {
        Cadence::Daily => {
            let today_at = at(today, hour, minute);
            if today_at > now {
                today_at
            } else {
                at(today + Days::new(1), hour, minute)
            }
        }
        Cadence::Weekly => {
            let back = u64::from(today.weekday().num_days_from_monday());
            let monday = today - Days::new(back);
            let monday_at = at(monday, hour, minute);
            if monday_at > now {
                monday_at
            } else {
                at(monday + Days::new(7), hour, minute)
            }
        }
        Cadence::Monthly => {
            let first = first_of_month(today);
            let first_at = at(first, hour, minute);
            if first_at > now {
                first_at
            } else {
                let next = first
                    .checked_add_months(Months::new(1))
                    .expect("calendar month within range");
                at(next, hour, minute)
            }
        }
    }
}

/// Map a local wall-clock instant to UTC, tolerating a DST gap/overlap.
/// A skipped wall time (spring-forward) resolves to the first existing instant
/// after it, so the schedule fires once rather than once per minute while the
/// clock is inside the gap.
fn local_to_utc(naive: NaiveDateTime) -> Option<DateTime<Utc>> {
    let resolve = |value: NaiveDateTime| match Local.from_local_datetime(&value) {
        LocalResult::Single(dt) | LocalResult::Ambiguous(dt, _) => Some(dt.with_timezone(&Utc)),
        LocalResult::None => None,
    };
    resolve(naive).or_else(|| {
        [30i64, 60, 90, 120]
            .into_iter()
            .find_map(|minutes| resolve(naive + chrono::Duration::minutes(minutes)))
    })
}

/// Whether a backup is due at `now`: the last one is older than the most
/// recent scheduled occurrence. With no backup yet, the first tick runs one
/// immediately (so enabling the feature does not wait for tomorrow).
#[must_use]
pub fn is_due(
    now: DateTime<Utc>,
    cadence: Cadence,
    time: (u32, u32),
    last: Option<DateTime<Utc>>,
) -> bool {
    let occurrence = last_occurrence(now.with_timezone(&Local).naive_local(), cadence, time);
    match (local_to_utc(occurrence), last) {
        (Some(occurrence), Some(last)) => occurrence > last,
        (Some(_), None) => true,
        // A wall-clock time the local zone skipped must not block backups, but
        // it must not storm either: without a resolvable occurrence, at most
        // one run per day.
        (None, Some(last)) => now - last >= chrono::Duration::hours(23),
        (None, None) => true,
    }
}

/// `next_occurrence` expressed in UTC, for the admin status payload.
#[must_use]
pub fn next_run(now: DateTime<Utc>, cadence: Cadence, time: (u32, u32)) -> Option<DateTime<Utc>> {
    let next = next_occurrence(now.with_timezone(&Local).naive_local(), cadence, time);
    local_to_utc(next).or_else(|| Some(now + chrono::Duration::days(1)))
}

fn company_name(root: &Path, cfg: &AppConfig, env: &dyn Fn(&str) -> Option<String>) -> String {
    let profile = std::fs::read(root.join("org_profile.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<crate::domain::OrgProfile>(&bytes).ok())
        .unwrap_or_default();
    let fallback = cfg.get_str("org_name", env);
    crate::pdf::Org::resolve(&profile, &fallback).name
}

/// Environment lookup shared with `AppState` (the job resolves the same
/// `TATU_*` keys as the request handlers).
pub type EnvLookup = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

/// The scheduler job behind Settings → Backup. It fires every minute, reads the
/// live configuration on each tick (so an edit applies without a restart) and
/// writes at most one archive per scheduled occurrence.
pub struct BackupJob {
    root: PathBuf,
    cfg: Arc<Mutex<Arc<AppConfig>>>,
    env: EnvLookup,
    audit: Arc<crate::audit::AuditLog>,
}

impl BackupJob {
    #[must_use]
    pub fn new(
        root: impl Into<PathBuf>,
        cfg: Arc<Mutex<Arc<AppConfig>>>,
        env: EnvLookup,
        audit: Arc<crate::audit::AuditLog>,
    ) -> Self {
        Self {
            root: root.into(),
            cfg,
            env,
            audit,
        }
    }
}

impl Job for BackupJob {
    fn name(&self) -> &str {
        "backup"
    }

    fn interval_secs(&self) -> i64 {
        60
    }

    fn run(&self, now: DateTime<Utc>) {
        let cfg = match self.cfg.lock() {
            Ok(guard) => guard.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        };
        let env = |key: &str| (self.env)(key);
        if !cfg.get_bool("backup_enabled", &env) {
            return;
        }
        let cadence =
            Cadence::parse(&cfg.get_str("backup_cadence", &env)).unwrap_or(Cadence::Daily);
        let time = crate::appconfig::parse_time_of_day(&cfg.get_str("backup_time", &env))
            .unwrap_or((2, 0));
        let destination = configured_directory(&self.root, &env);
        let last = match list_backups_in(&destination) {
            Ok(files) => files.first().map(|file| file.created_at),
            Err(error) => {
                tracing::warn!(error = %error, "backup directory could not be read; skipping this tick");
                return;
            }
        };
        if !is_due(now, cadence, time, last) {
            return;
        }
        let company = company_name(&self.root, &cfg, &env);
        match run_in(&destination, &self.root, &company, now) {
            Ok(created) => {
                self.audit.record("backup_created", &created.file_name, now);
                tracing::info!(file = %created.file_name, "scheduled backup created");
            }
            Err(error) => tracing::warn!(error = %error, "scheduled backup failed"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed_data(dir: &Path) {
        std::fs::create_dir_all(dir.join("invoices")).unwrap();
        std::fs::write(dir.join("users.json"), b"[{\"id\":\"a\"}]").unwrap();
        std::fs::write(dir.join("invoices/INV-1.json"), b"{\"total\":99}").unwrap();
        std::fs::write(dir.join("secrets.bin"), b"encrypted blob").unwrap();
        std::fs::write(dir.join("config.json"), b"{\"reminder_days\":10}").unwrap();
        // transient file must be skipped
        std::fs::write(dir.join("users.json.tmp"), b"partial").unwrap();
    }

    #[test]
    fn archived_pdf_round_trips_with_intact_sha256() {
        // #113: the archive under invoices/ is a non-JSON file the backup
        // walk already covers; prove the PDF bytes survive untouched and the
        // manifest checksum matches the original.
        use sha2::{Digest, Sha256};
        let src = tempfile::tempdir().unwrap();
        let hold = tempfile::tempdir().unwrap();
        seed_data(src.path());
        let pdf = b"%PDF-1.4\n\x80\x81\xfe rest of the invoice document".to_vec();
        std::fs::write(src.path().join("invoices/INV-1.pdf"), &pdf).unwrap();
        let original_sha = Sha256::digest(&pdf);

        let archive = hold.path().join("out.tar.gz");
        create(src.path(), &archive).unwrap();
        let dst = tempfile::tempdir().unwrap();
        restore(&archive, dst.path(), false).unwrap();

        let restored = std::fs::read(dst.path().join("invoices/INV-1.pdf")).unwrap();
        assert_eq!(restored, pdf, "PDF bytes round-trip byte-for-byte");
        assert_eq!(Sha256::digest(&restored), original_sha);
        // `restore()` verifies each file's manifest sha256 as it streams (it
        // returns Corrupt on any mismatch), so a clean restore is the proof
        // the archived PDF's checksum survived the round-trip.
        let _ = &hold;
    }

    #[test]
    fn backup_restore_roundtrip_with_checksums() {
        let src = tempfile::tempdir().unwrap();
        let hold = tempfile::tempdir().unwrap(); // archive lives outside the backup root
        seed_data(src.path());
        let archive = hold.path().join("out.tar.gz");
        let n = create(src.path(), &archive).unwrap();
        assert_eq!(n, 4, "tmp file excluded");

        let dst = tempfile::tempdir().unwrap();
        let notes = restore(&archive, dst.path(), false).unwrap();
        assert!(
            std::fs::read_to_string(dst.path().join("users.json"))
                .unwrap()
                .contains("a")
        );
        assert!(
            std::fs::read_to_string(dst.path().join("invoices/INV-1.json"))
                .unwrap()
                .contains("99")
        );
        assert_eq!(
            std::fs::read(dst.path().join("secrets.bin")).unwrap(),
            b"encrypted blob"
        );
        assert!(!dst.path().join("users.json.tmp").exists());
        assert!(notes.iter().any(|x| x.contains("secrets.bin")));
        let _ = &hold;

        // Restore over a non-empty dir is refused without --force.
        assert!(matches!(
            restore(&archive, dst.path(), false),
            Err(BackupError::NotEmpty)
        ));
        assert!(restore(&archive, dst.path(), true).is_ok());
        let _ = std::fs::remove_file(&archive);
    }

    #[test]
    fn tampered_archive_fails_verification() {
        let src = tempfile::tempdir().unwrap();
        seed_data(src.path());
        let archive = src.path().join("out.tar.gz");
        create(src.path(), &archive).unwrap();
        // Flip a byte inside the payload region (past headers of file #2).
        let bytes = std::fs::read(&archive).unwrap();
        // Re-pack raw: decode gz, edit, re-encode.
        let mut plain = Vec::new();
        GzDecoder::new(bytes.as_slice())
            .read_to_end(&mut plain)
            .unwrap();
        let pos = plain
            .windows(b"\"total\":99".len())
            .position(|w| w == b"\"total\":99")
            .unwrap();
        plain[pos + 9] = b'8'; // 99 -> 98
        let mut enc = std::io::Cursor::new(Vec::new());
        {
            let mut g = GzEncoder::new(&mut enc, Compression::default());
            g.write_all(&plain).unwrap();
        }
        std::fs::write(&archive, enc.into_inner()).unwrap();
        let dst = tempfile::tempdir().unwrap();
        assert!(matches!(
            restore(&archive, dst.path(), false),
            Err(BackupError::Corrupt(_))
        ));
    }

    #[test]
    fn path_traversal_in_archive_is_rejected() {
        assert!(sanitize("a/b.json").is_ok());
        assert!(matches!(
            sanitize("../evil"),
            Err(BackupError::UnsafePath(_))
        ));
        assert!(matches!(
            sanitize("/etc/passwd"),
            Err(BackupError::UnsafePath(_))
        ));
    }

    // ------------------------------------------------------ ZIP backups ----

    #[test]
    fn zip_round_trips_data_files_without_transient_artefacts() {
        let src = tempfile::tempdir().unwrap();
        let hold = tempfile::tempdir().unwrap();
        seed_data(src.path());
        let destination = hold.path().join("Backup");

        let created = run_in(
            &destination,
            src.path(),
            "Capyabara Solutions Ltda",
            Utc::now(),
        )
        .unwrap();
        assert!(
            created.file_name.contains(FILE_MARKER)
                && created.file_name.ends_with("_capyabarasolutionsltda.zip"),
            "unexpected name: {}",
            created.file_name
        );
        let archive = destination.join(&created.file_name);
        assert!(archive.is_file());
        assert_eq!(
            created.size_bytes,
            std::fs::metadata(&archive).unwrap().len()
        );

        let mut zip = zip::ZipArchive::new(std::fs::File::open(&archive).unwrap()).unwrap();
        let mut names: Vec<String> = (0..zip.len())
            .map(|i| zip.by_index(i).unwrap().name().unwrap().to_string())
            .collect();
        names.sort();
        assert_eq!(
            names,
            vec![
                "config.json".to_string(),
                "invoices/INV-1.json".to_string(),
                "secrets.bin".to_string(),
                "users.json".to_string(),
            ],
            "tmp/lock files are excluded and files stay at the archive root"
        );
        let mut body = String::new();
        zip.by_name("users.json")
            .unwrap()
            .read_to_string(&mut body)
            .unwrap();
        assert!(body.contains("\"a\""));

        let listed = list_backups_in(&destination).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].file_name, created.file_name);
    }

    #[test]
    fn backups_in_the_same_second_get_distinct_names() {
        let src = tempfile::tempdir().unwrap();
        let hold = tempfile::tempdir().unwrap();
        seed_data(src.path());
        let destination = hold.path().join("Backup");
        let now = Utc::now();
        let first = run_in(&destination, src.path(), "Tatu", now).unwrap();
        let second = run_in(&destination, src.path(), "Tatu", now).unwrap();
        assert_ne!(first.file_name, second.file_name);
        assert!(second.file_name.ends_with("-1.zip"));
    }

    #[test]
    fn backup_names_are_slugged_and_portable() {
        assert_eq!(
            company_slug("Capyabara Solutions Ltda"),
            "capyabarasolutionsltda"
        );
        assert_eq!(company_slug("  "), "tatutime");
        assert_eq!(company_slug("A/B:C"), "abc");
        assert_eq!(directory(Path::new("Data")), PathBuf::from("Backup"));
        assert_eq!(
            directory(Path::new("/tmp/install/Data")),
            PathBuf::from("/tmp/install/Backup")
        );
    }

    #[test]
    fn schedule_occurrences_follow_the_cadence() {
        use chrono::NaiveDate;
        let friday = NaiveDate::from_ymd_opt(2026, 10, 23)
            .unwrap()
            .and_hms_opt(10, 30, 0)
            .unwrap();
        // Daily: today when the time has passed, yesterday before it.
        assert_eq!(
            last_occurrence(friday, Cadence::Daily, (2, 0)),
            NaiveDate::from_ymd_opt(2026, 10, 23)
                .unwrap()
                .and_hms_opt(2, 0, 0)
                .unwrap()
        );
        let early = NaiveDate::from_ymd_opt(2026, 10, 23)
            .unwrap()
            .and_hms_opt(1, 0, 0)
            .unwrap();
        assert_eq!(
            last_occurrence(early, Cadence::Daily, (2, 0)),
            NaiveDate::from_ymd_opt(2026, 10, 22)
                .unwrap()
                .and_hms_opt(2, 0, 0)
                .unwrap()
        );
        // Weekly: Monday of this week, or the previous Monday before its time.
        assert_eq!(
            last_occurrence(friday, Cadence::Weekly, (2, 0)),
            NaiveDate::from_ymd_opt(2026, 10, 19)
                .unwrap()
                .and_hms_opt(2, 0, 0)
                .unwrap()
        );
        let monday_early = NaiveDate::from_ymd_opt(2026, 10, 19)
            .unwrap()
            .and_hms_opt(1, 0, 0)
            .unwrap();
        assert_eq!(
            last_occurrence(monday_early, Cadence::Weekly, (2, 0)),
            NaiveDate::from_ymd_opt(2026, 10, 12)
                .unwrap()
                .and_hms_opt(2, 0, 0)
                .unwrap()
        );
        // Monthly: the 1st of this month, or the previous month's 1st.
        assert_eq!(
            last_occurrence(friday, Cadence::Monthly, (2, 0)),
            NaiveDate::from_ymd_opt(2026, 10, 1)
                .unwrap()
                .and_hms_opt(2, 0, 0)
                .unwrap()
        );
        let first_early = NaiveDate::from_ymd_opt(2026, 10, 1)
            .unwrap()
            .and_hms_opt(1, 0, 0)
            .unwrap();
        assert_eq!(
            last_occurrence(first_early, Cadence::Monthly, (2, 0)),
            NaiveDate::from_ymd_opt(2026, 9, 1)
                .unwrap()
                .and_hms_opt(2, 0, 0)
                .unwrap()
        );
        // Next run is strictly after now.
        assert_eq!(
            next_occurrence(friday, Cadence::Daily, (2, 0)),
            NaiveDate::from_ymd_opt(2026, 10, 24)
                .unwrap()
                .and_hms_opt(2, 0, 0)
                .unwrap()
        );
    }

    #[test]
    fn is_due_compares_the_last_backup_with_the_last_occurrence() {
        let now = Utc::now();
        assert!(is_due(now, Cadence::Daily, (2, 0), None));
        assert!(!is_due(now, Cadence::Daily, (2, 0), Some(now)));
        assert!(is_due(
            now,
            Cadence::Daily,
            (2, 0),
            Some(now - chrono::Duration::days(3))
        ));
    }

    #[test]
    fn scheduled_job_reads_live_config_and_writes_one_archive() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("data");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("config.json"),
            r#"{"backup_enabled": true, "backup_cadence": "daily", "backup_time": "02:00"}"#,
        )
        .unwrap();
        std::fs::write(
            root.join("org_profile.json"),
            r#"{"name": "Capyabara Solutions Ltda"}"#,
        )
        .unwrap();
        let cfg = AppConfig::load(&root).unwrap();
        let audit = Arc::new(crate::audit::AuditLog::new(&root));
        let env: EnvLookup = Arc::new(|_| None);
        let job = BackupJob::new(
            root.clone(),
            Arc::new(Mutex::new(Arc::new(cfg))),
            env,
            audit,
        );

        job.run(Utc::now());
        let files = list_backups_in(&dir.path().join("Backup")).unwrap();
        assert_eq!(files.len(), 1, "the enabled job writes one archive");
        assert!(
            files[0]
                .file_name
                .contains("_tatutime-backup_capyabarasolutionsltda.zip"),
            "unexpected name: {}",
            files[0].file_name
        );

        // Disabling the knob stops the job: a later tick writes nothing.
        std::fs::write(
            root.join("config.json"),
            r#"{"backup_enabled": false, "backup_cadence": "daily", "backup_time": "02:00"}"#,
        )
        .unwrap();
        let disabled = AppConfig::load(&root).unwrap();
        let job = BackupJob::new(
            root,
            Arc::new(Mutex::new(Arc::new(disabled))),
            Arc::new(|_| None),
            Arc::new(crate::audit::AuditLog::new(dir.path())),
        );
        job.run(Utc::now() + chrono::Duration::minutes(1));
        assert_eq!(
            list_backups_in(&dir.path().join("Backup")).unwrap().len(),
            1
        );
    }
}
