use crate::support::*;

use axum::http::StatusCode;
use serde_json::json;

#[tokio::test]
async fn delete_customer_refused_when_project_docs_are_unreadable() {
    // #185: `delete_customer` swallowed a failed project listing as "empty"
    // and then remove_dir_all'd the customer tree INCLUDING the unreadable
    // project docs. An unlistable collection must refuse the deletion and
    // leave every file intact.
    let (app, d) = app().await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap().to_string();
    new_project(&app, &cid, "P1", json!({})).await;
    let project_path = d
        .path()
        .join(format!("data/customers/{cid}/projects/P1.json"));
    std::fs::write(&project_path, b"{ this is not json").unwrap();

    let (s, b) = json_req(&app, "DELETE", &format!("/api/customers/{cid}"), None).await;
    assert_eq!(s, StatusCode::INTERNAL_SERVER_ERROR, "{b}");
    assert!(
        d.path().join(format!("data/customers/{cid}.json")).exists(),
        "customer document must survive a refused delete"
    );
    assert!(
        project_path.exists(),
        "project documents must not be deleted when they could not be listed"
    );
}

#[test]
fn concurrent_writer_times_out_with_lock_busy() {
    use fs2::FileExt;
    use std::time::Duration;
    let dir = tempfile::tempdir().unwrap();
    // A store that gives up quickly when the writer lock is held.
    let store = tucano_time::store::Store::with_lock_timeout(
        dir.path().join("data"),
        Duration::from_millis(50),
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join("data")).unwrap();
    // Simulate another process holding the advisory writer lock.
    let held = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(dir.path().join("data").join(".tucanotime.lock"))
        .unwrap();
    held.lock_exclusive().unwrap();

    let cust = tucano_time::domain::Customer {
        id: uuid::Uuid::new_v4(),
        name: "X".into(),
        currency: tucano_time::domain::Currency("EUR".into()),
        default_rate_minor: 1,
        active: true,
        email: String::new(),
        payment_terms: None,
        invoice_notes: String::new(),
        invoice_subject: String::new(),
        address: None,
        contacts: vec![],
        tax_hundredths: 0,
        discount_hundredths: 0,

        payment_details: None,
    };
    let err = store.put_customer(&cust).expect_err("should time out");
    assert!(
        matches!(err, tucano_time::store::StoreError::LockTimeout),
        "{err:?}"
    );

    // Release -> the same write now succeeds.
    held.unlock().unwrap();
    drop(held);
    store
        .put_customer(&cust)
        .expect("write succeeds once lock is free");
}

#[tokio::test]
async fn store_fault_during_entry_validation_is_500_not_422() {
    // #187: validate_entry_refs' `.ok().flatten()` turned any store failure
    // into a bogus "customer does not exist" 422. A corrupt customer doc must
    // surface as a 500 — and, since QA-12, as the distinguishable
    // `corrupt_document` rather than the opaque `internal` (no internals
    // leaked either way).
    let (app, d) = app().await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap().to_string();
    new_project(&app, &cid, "P1", json!({})).await;
    std::fs::write(
        d.path().join(format!("data/customers/{cid}.json")),
        b"{ not json",
    )
    .unwrap();
    let (s, b) = json_req(
        &app,
        "POST",
        "/api/entries",
        Some(entry_body(&cid, "P1", json!(1), "2026-10-02")),
    )
    .await;
    assert_eq!(s, StatusCode::INTERNAL_SERVER_ERROR, "{b}");
    assert_eq!(b["error"]["code"], "corrupt_document");
    let text = b.to_string();
    assert!(
        !text.contains("json") && !text.contains("data/"),
        "no internals: {text}"
    );
}

/// #262(4): the `.server.lock` single-instance guard (the data-corruption
/// guard it exists for). A second instance against the same data dir must be
/// refused; releasing the first lets a new one start.
#[test]
fn server_lock_refuses_a_second_instance() {
    let dir = tempfile::tempdir().unwrap();
    let first = tucano_time::acquire_server_lock(dir.path()).expect("first lock");
    let second = tucano_time::acquire_server_lock(dir.path());
    assert!(second.is_err(), "a second instance must be refused");
    drop(first);
    let third = tucano_time::acquire_server_lock(dir.path());
    assert!(third.is_ok(), "the lock must be reacquirable after release");
}

/// #336: the storage-layout guard — `tests::route_parity` (`src/lib.rs`) for
/// the data dir. `AGENTS.md`'s "Architecture invariants" bullet states the
/// layout in prose while `src/store.rs` derives it (`Entity::DIR` impls, the
/// customer tree, the per-day entry folders, `notifications/`); nothing tied the
/// two, so the two lists could drift apart silently (review §2.3).
///
/// WHAT IT PINS
/// * the collection set, **both ways**: every [`tucano_time::store::DOC_COLLECTIONS`]
///   row must be named in the bullet and every directory the bullet names must
///   have a row behind it — so a collection that is never documented, or a
///   documented directory with no `Entity::DIR`/named constant behind it, fails
///   with the drift named;
/// * the nesting and document stems the store *writes*: a temp-dir store is
///   driven through its public writers (`put_user`, `put_customer`,
///   `put_project`, `put_task`, `put_entry`, `push_notification`,
///   `create_invoice`) and the relative paths that appear are compared both to
///   the paths built by filling the placeholders of the documented shapes and to
///   the whole documented directory set — a renamed folder, a changed nesting
///   level or a changed document stem fails.
///
/// WHAT IT DOES NOT COVER
/// * it pins the **mapping**, never a live filesystem snapshot: nothing reads
///   the developer's `data/`, and the directory walk collects folders only, so
///   transient files (`*.tmp`, `.defaults-seeded`, `.tucanotime.lock`) cannot
///   make it flaky;
/// * the root-level single files (`audit.log`, `revoked.json`,
///   `scheduler.json`, `secrets.bin`, `config.json`, `session.key`,
///   `.server.lock`, `sync/accounting.json`, the `*.idx.*` indexes and
///   `invoices/.seq.json`) are not a closed enumeration here: the ones this test
///   materialises are checked against their documented shape, and the modules
///   that own the rest keep their own tests. `.tucanotime.lock` (#62) is
///   deliberately outside the guard;
/// * `DOC_COLLECTIONS` is a hand-kept list beside the `Entity` impls: a brand
///   new collection is caught once it is added there. That is the deliberate act
///   the guard is built around — trait impls cannot be enumerated from outside
///   the crate, so the list is the seam, and it is documented as such in
///   `src/store.rs`.
mod storage_layout {
    use std::path::Path;

    use tucano_time::auth::{Role, User};
    use tucano_time::domain::{
        Currency, Customer, Entry, Hours, Invoice, InvoiceStatus, Notification, Project,
        ProjectCode, Task,
    };
    use tucano_time::store::Store;
    use uuid::Uuid;

    /// The repository rules file, read whole: the guard is about what it says.
    const AGENTS_MD: &str = include_str!("../../AGENTS.md");

    /// The "Storage layout below `TUCANO_DATA_DIR`" bullet, from its marker to
    /// the next top-level ("- ") bullet. Panics if the bullet is gone — itself
    /// the drift the guard exists to refuse.
    fn layout_bullet() -> &'static str {
        let start = AGENTS_MD
            .find("- Storage layout below")
            .expect("AGENTS.md no longer states the storage layout (#336)");
        let rest = &AGENTS_MD[start..];
        let end = rest[1..].find("\n- ").map_or(rest.len(), |i| i + 1);
        &rest[..end]
    }

    /// Every `` `backticked` `` token in `text`, in order of appearance.
    fn backticked(text: &str) -> Vec<&str> {
        let mut out = Vec::new();
        let mut rest = text;
        while let Some(open) = rest.find('`') {
            let after = &rest[open + 1..];
            let Some(close) = after.find('`') else { break };
            out.push(&after[..close]);
            rest = &after[close + 1..];
        }
        out
    }

    /// Collection directories as the bullet names them: the per-document form
    /// `<dir>/<id>.json` (which also matches the date-sharded
    /// `entries/<YYYY-MM-DD>/<id>.json`) and the bare doc-folder form `<dir>/`.
    /// Everything else the bullet lists — the side-car files,
    /// `sync/accounting.json`, `*.idx.*` — is not a collection directory and
    /// filters out here.
    fn documented_collections() -> Vec<String> {
        let mut out: Vec<String> = backticked(layout_bullet())
            .into_iter()
            .filter_map(|token| {
                if let Some(dir) = token.strip_suffix('/') {
                    // `retainers/`, `notifications/` — a bare folder name.
                    if !dir.is_empty() && !dir.contains('/') {
                        Some(dir.to_string())
                    } else {
                        None
                    }
                } else if token.contains("<id>.json") {
                    // `customers/<id>.json`, `entries/<YYYY-MM-DD>/<id>.json`.
                    token.split_once('/').map(|(dir, _)| dir.to_string())
                } else {
                    None
                }
            })
            .collect();
        out.sort();
        out.dedup();
        out
    }

    /// Substitute the placeholders of a documented shape. Refuses to hand back
    /// anything still holding a `<…>` placeholder, so a shape that gains one
    /// cannot quietly compare equal to a real path.
    fn fill(shape: &str, subs: &[(&str, &str)]) -> String {
        let mut out = shape.to_string();
        for (placeholder, value) in subs {
            out = out.replace(placeholder, value);
        }
        assert!(!out.contains('<'), "unsubstituted placeholder in `{out}`");
        out
    }

    /// Every directory below `root` as sorted `/`-joined relative paths.
    fn dirs_under(root: &Path) -> Vec<String> {
        fn walk(root: &Path, rel: &str, out: &mut Vec<String>) {
            let dir = if rel.is_empty() {
                root.to_path_buf()
            } else {
                root.join(rel)
            };
            for entry in std::fs::read_dir(&dir).expect("read data dir") {
                let entry = entry.expect("dir entry");
                if !entry.file_type().expect("file type").is_dir() {
                    continue;
                }
                let name = entry.file_name().to_string_lossy().to_string();
                let child = if rel.is_empty() {
                    name
                } else {
                    format!("{rel}/{name}")
                };
                walk(root, &child, out);
                out.push(child);
            }
        }
        let mut out = Vec::new();
        walk(root, "", &mut out);
        out.sort();
        out
    }

    /// #336: the documented collection set and the store's own registry, held to
    /// each other name for name.
    #[test]
    fn collections_match_agents_md() {
        let documented = documented_collections();
        let mut store_side: Vec<String> = tucano_time::store::DOC_COLLECTIONS
            .iter()
            .map(|dir| (*dir).to_string())
            .collect();
        store_side.sort();
        store_side.dedup();

        let undocumented: Vec<&String> = store_side
            .iter()
            .filter(|dir| !documented.contains(*dir))
            .collect();
        let unbacked: Vec<&String> = documented
            .iter()
            .filter(|dir| !store_side.contains(*dir))
            .collect();
        assert!(
            undocumented.is_empty() && unbacked.is_empty(),
            "#336 storage-layout drift between src/store.rs and AGENTS.md:\n  \
             store collections missing from the AGENTS.md bullet: {undocumented:?}\n  \
             AGENTS.md collections with no Entity::DIR / store row behind them: {unbacked:?}"
        );
    }

    /// #336: the documented nesting and document stems, materialised through the
    /// store's public writers in a temp dir.
    #[test]
    fn store_writes_the_documented_nesting() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("data")).unwrap();
        let root = store.root().to_path_buf();

        let user = User {
            id: Uuid::new_v4(),
            name: "Layout".into(),
            email: "layout@test.local".into(),
            role: Role::Admin,
            active: true,
            default_rate_minor: 0,
            cost_rate_minor: 0,
            password_hash: String::new(),
            created_at: chrono::Utc::now(),
            session_version: 1,
        };
        store.put_user(&user).unwrap();

        let customer = Customer {
            id: Uuid::new_v4(),
            name: "Layout Customer".into(),
            currency: Currency("EUR".into()),
            default_rate_minor: 9000,
            active: true,
            email: String::new(),
            payment_terms: None,
            invoice_notes: String::new(),
            invoice_subject: String::new(),
            address: None,
            contacts: vec![],
            tax_hundredths: 0,
            discount_hundredths: 0,

            payment_details: None,
        };
        store.put_customer(&customer).unwrap();

        let code = "ACME-1";
        store
            .put_project(&Project {
                customer_id: customer.id,
                code: ProjectCode::parse(code).unwrap(),
                name: "Layout project".into(),
                currency: Currency("EUR".into()),
                rate_minor: 9000,
                active: true,
                budget_hours: None,
                budget_amount_minor: None,
            })
            .unwrap();

        let task_code = "DEV";
        store
            .put_task(&Task {
                customer_id: customer.id,
                project_code: ProjectCode::parse(code).unwrap(),
                code: ProjectCode::parse(task_code).unwrap(),
                name: "Development".into(),
                active: true,
            })
            .unwrap();

        let date = chrono::NaiveDate::from_ymd_opt(2026, 10, 9).unwrap();
        let entry = Entry {
            id: Uuid::new_v4(),
            date,
            customer_id: customer.id,
            user_id: Some(user.id),
            project_code: ProjectCode::parse(code).unwrap(),
            task_code: Some(ProjectCode::parse(task_code).unwrap()),
            hours: Hours(200),
            note: String::new(),
            billable: true,
            source: Default::default(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        store.put_entry(&entry).unwrap();

        store
            .push_notification(
                user.id,
                &Notification {
                    id: Uuid::new_v4(),
                    kind: "layout".into(),
                    title: "Layout".into(),
                    body: "Layout".into(),
                    created_at: chrono::Utc::now(),
                    read: false,
                },
            )
            .unwrap();

        let invoice = store
            .create_invoice(Invoice {
                id: Uuid::new_v4(),
                number: String::new(),
                customer_id: customer.id,
                currency: Currency("EUR".into()),
                period_from: date,
                period_to: date,
                lines: vec![],
                total_minor: 0,
                status: InvoiceStatus::Draft,
                created_at: chrono::Utc::now(),
                issued_at: None,
                due_date: None,
                paid_at: None,
                payment_reference: String::new(),
                pdf: None,
                payments: vec![],
                write_off_reason: String::new(),
                written_off_at: None,
                tax_hundredths: 0,
                discount_hundredths: 0,
            })
            .unwrap();

        // The documentation must still spell every shape the store just wrote —
        // otherwise the expectations below would be pinning a stale string.
        let bullet = layout_bullet();
        let shapes = [
            "customers/<id>.json",
            "customers/<id>/projects/<CODE>.json",
            "customers/<id>/projects/<CODE>/tasks/<TASK>.json",
            "entries/<YYYY-MM-DD>/<id>.json",
            "users/<id>.json",
            "invoices/<id>.json",
            "invoices/.seq.json",
            "*.idx.*",
        ];
        for shape in shapes {
            assert!(
                bullet.contains(shape),
                "AGENTS.md stopped documenting `{shape}` (#336) — the store writes it"
            );
        }

        let cid = customer.id.to_string();
        let user_id = user.id.to_string();
        let invoice_id = invoice.id.to_string();
        let entry_id = entry.id.to_string();
        let day = date.format("%Y-%m-%d").to_string();

        // Documents, at the paths the documented shapes expand to.
        let expected_documents = [
            fill("users/<id>.json", &[("<id>", &user_id)]),
            fill("customers/<id>.json", &[("<id>", &cid)]),
            fill(
                "customers/<id>/projects/<CODE>.json",
                &[("<id>", &cid), ("<CODE>", code)],
            ),
            fill(
                "customers/<id>/projects/<CODE>/tasks/<TASK>.json",
                &[("<id>", &cid), ("<CODE>", code), ("<TASK>", task_code)],
            ),
            fill(
                "entries/<YYYY-MM-DD>/<id>.json",
                &[("<YYYY-MM-DD>", &day), ("<id>", &entry_id)],
            ),
            fill("invoices/<id>.json", &[("<id>", &invoice_id)]),
            // The bullet names the folder only; the store's stem inside it is
            // per-user, like `timers/`. Pinned here so it cannot move alone.
            format!("notifications/{user_id}.json"),
            "invoices/.seq.json".to_string(),
        ];
        for rel in &expected_documents {
            assert!(
                root.join(rel).is_file(),
                "the store did not write the documented `{rel}` (#336)"
            );
        }

        // The `*.idx.*` side indexes the bullet documents by glob: one per
        // indexed collection, both matching the glob.
        for rel in ["invoices/.idx.numbers.json", "users/.idx.emails.json"] {
            assert!(
                root.join(rel).is_file(),
                "the store did not write the documented side index `{rel}` (#336)"
            );
            let name = rel.rsplit('/').next().unwrap();
            assert!(
                name.starts_with('.') && name.contains(".idx."),
                "the documented `*.idx.*` shape (#336): {rel}"
            );
        }

        // Directories: the whole set, so an undocumented folder fails here even
        // before it grows a document.
        let mut expected_dirs = vec![
            "categories".to_string(),
            "customers".to_string(),
            format!("customers/{cid}"),
            format!("customers/{cid}/projects"),
            format!("customers/{cid}/projects/{code}"),
            format!("customers/{cid}/projects/{code}/tasks"),
            "entries".to_string(),
            format!("entries/{day}"),
            "expenses".to_string(),
            "invoices".to_string(),
            "notifications".to_string(),
            "users".to_string(),
        ];
        expected_dirs.sort();
        assert_eq!(
            dirs_under(&root),
            expected_dirs,
            "#336: the store's directory layout drifted from AGENTS.md"
        );
    }
}

/// #349: `archive_entries_before`, the retention primitive ADR-003 names as its
/// precondition 1 — the lock/invoice refusal rule, the byte-identical move into
/// the cold tree, the per-run sha256 manifest, the #335 index shrink, and the dry
/// run that writes nothing.
///
/// WHAT THESE PIN
/// * a day holding an entry frozen by the lock seam, or one on a *non-draft*
///   invoice, is refused whole: nothing moves, the data dir is byte-identical
///   and the refusal is reported with its reason (a lock state that cannot be
///   verified refuses every candidate, fail closed, #185);
/// * a clean day moves byte for byte into `archive/entries/<YYYY-MM>/<day>/`, the
///   manifest verifies against the moved bytes, and the #335 index stops naming
///   the relocated day while keeping every hot day;
/// * the cutoff is strict (`date < cutoff`), a cold copy is never overwritten,
///   anything under `entries/` that is not a well-formed day folder is refused,
///   a dry run writes nothing at all, and a second pass relocates nothing.
mod entries_archive {
    use std::collections::HashSet;
    use std::path::{Path, PathBuf};

    use axum::http::StatusCode;
    use serde_json::{Value, json};
    use sha2::{Digest, Sha256};
    use tucano_time::lock::{EntryLock, LockReason, LockUnavailable, NoLocks};
    use tucano_time::store::{ArchiveRefusal, Store};
    use uuid::Uuid;

    use crate::support::{Client, LockOne, app, entry_body, json_req, new_customer, new_project};

    /// The two seeded days: one old enough to be a candidate, one newer than the
    /// cutoff that must never be touched.
    const DAY: &str = "2026-08-03";
    const LATER: &str = "2026-10-09";
    const CUTOFF: &str = "2026-09-01";

    fn cutoff() -> chrono::NaiveDate {
        chrono::NaiveDate::parse_from_str(CUTOFF, "%Y-%m-%d").expect("cutoff")
    }

    /// The data root below the temp dir the harness hands out.
    fn data_root(dir: &tempfile::TempDir) -> PathBuf {
        dir.path().join("data")
    }

    fn entry_dir(data: &Path, date: &str) -> PathBuf {
        data.join("entries").join(date)
    }

    fn index_path(data: &Path) -> PathBuf {
        data.join("entries").join(".idx.users.json")
    }

    /// Lowercase hex of a sha256 — the encoding the store's manifest rows carry,
    /// re-derived here so the test verifies them independently of the store.
    fn sha256_hex(bytes: &[u8]) -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let digest = Sha256::digest(bytes);
        let mut out = String::with_capacity(digest.len() * 2);
        for byte in digest {
            out.push(HEX[(byte >> 4) as usize] as char);
            out.push(HEX[(byte & 0x0f) as usize] as char);
        }
        out
    }

    /// Every file under `dir` as (relative path, bytes), sorted — the strongest
    /// "nothing was written" assertion available to a test.
    fn snapshot(dir: &Path) -> Vec<(String, Vec<u8>)> {
        fn walk(dir: &Path, base: &Path, out: &mut Vec<(String, Vec<u8>)>) {
            let Ok(children) = std::fs::read_dir(dir) else {
                return;
            };
            for child in children.filter_map(|c| c.ok()) {
                let path = child.path();
                if path.is_dir() {
                    walk(&path, base, out);
                } else {
                    let rel = path
                        .strip_prefix(base)
                        .map(|p| p.to_string_lossy().replace('\\', "/"))
                        .unwrap_or_default();
                    out.push((rel, std::fs::read(&path).expect("read file")));
                }
            }
        }
        let mut out = Vec::new();
        walk(dir, dir, &mut out);
        out.sort();
        out
    }

    /// The shared fixture: a customer with project `P1`, returned as the customer
    /// token the entry and invoice payloads need.
    async fn fixture(client: &Client) -> String {
        let customer = new_customer(client, "ACME", "EUR", 6000).await;
        let token = customer["id"].as_str().expect("customer id").to_string();
        new_project(client, &token, "P1", json!({})).await;
        token
    }

    /// One billable entry on `date`; returns its id.
    async fn entry_on(client: &Client, token: &str, date: &str, hours: u32) -> String {
        let (status, created) = json_req(
            client,
            "POST",
            "/api/entries",
            Some(entry_body(token, "P1", json!(hours), date)),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "create entry: {created}");
        created["id"].as_str().expect("entry id").to_string()
    }

    /// A lock seam that cannot answer (#185): every candidate day must be refused
    /// rather than archived behind an unverified check.
    struct Unverifiable;

    impl EntryLock for Unverifiable {
        fn entry_lock(&self, _entry_id: Uuid) -> Result<Option<LockReason>, LockUnavailable> {
            Err(LockUnavailable("lock state unreadable".into()))
        }

        fn locked_entries(&self) -> Result<HashSet<Uuid>, LockUnavailable> {
            Err(LockUnavailable("lock state unreadable".into()))
        }
    }

    #[tokio::test]
    async fn a_clean_day_moves_byte_identically_and_its_manifest_verifies() {
        let (client, d) = app().await;
        let token = fixture(&client).await;
        let first = entry_on(&client, &token, DAY, 3).await;
        let second = entry_on(&client, &token, DAY, 4).await;
        entry_on(&client, &token, LATER, 1).await;

        let data = data_root(&d);
        let store = Store::open(&data).expect("store");
        let moved = snapshot(&entry_dir(&data, DAY));
        assert_eq!(moved.len(), 2, "the day holds both documents");

        let report = store
            .archive_entries_before(cutoff(), false, &NoLocks)
            .expect("the pass runs");

        assert!(!report.dry_run);
        assert!(report.skipped.is_empty(), "{:?}", report.skipped);
        assert_eq!(report.skipped_entries, 0);
        assert_eq!(report.days.len(), 1, "{:?}", report.days);
        assert_eq!(report.days[0].day, DAY);
        assert_eq!(report.days[0].documents, 2);
        assert_eq!(report.documents, 2);
        let bytes: u64 = moved.iter().map(|(_, b)| b.len() as u64).sum();
        assert_eq!(report.bytes, bytes);
        assert_eq!(report.days[0].bytes, bytes);

        // The candidate left the hot tree, the newer day stayed, and every
        // document is in the cold tree byte for byte.
        let cold = data.join("archive/entries/2026-08").join(DAY);
        assert!(
            !entry_dir(&data, DAY).exists(),
            "a moved day leaves the hot tree"
        );
        assert!(
            entry_dir(&data, LATER).is_dir(),
            "a day after the cutoff stays"
        );
        for (name, original) in &moved {
            assert_eq!(
                &std::fs::read(cold.join(name)).expect("cold copy"),
                original,
                "{name} moved byte-identically"
            );
        }
        for id in [&first, &second] {
            assert!(cold.join(format!("{id}.json")).is_file());
            assert!(!entry_dir(&data, DAY).join(format!("{id}.json")).exists());
        }

        // The per-run manifest verifies against the bytes that were moved, and
        // lives inside the cold tree (so a `--backup` carries it).
        let rel = report
            .manifest
            .clone()
            .expect("a pass that moved a day writes a manifest");
        assert!(rel.starts_with("archive/entries/.manifest."), "{rel}");
        let manifest: Value =
            serde_json::from_slice(&std::fs::read(data.join(&rel)).expect("manifest file"))
                .expect("manifest json");
        assert_eq!(manifest["app"], "tucano-time");
        assert_eq!(manifest["cutoff"], CUTOFF);
        let rows = manifest["files"].as_array().expect("files").clone();
        assert_eq!(rows.len(), 2, "{manifest}");
        for row in &rows {
            let path = row["path"].as_str().expect("path");
            assert!(
                path.starts_with(&format!("archive/entries/2026-08/{DAY}/")),
                "the manifest names the cold path: {path}"
            );
            let archived = std::fs::read(data.join(path)).expect("a manifest path is a real file");
            assert_eq!(row["size"].as_u64(), Some(archived.len() as u64), "{row}");
            let digest = sha256_hex(&archived);
            assert_eq!(row["sha256"].as_str(), Some(digest.as_str()), "{row}");
        }

        // The #335 index may only name days that are on disk: the relocated day's
        // key is gone, the hot day's slice survives.
        let index: Value =
            serde_json::from_slice(&std::fs::read(index_path(&data)).expect("index"))
                .expect("index json");
        assert!(
            index["days"].get(DAY).is_none(),
            "the index must not name a cold day: {index}"
        );
        assert!(
            index["days"].get(LATER).is_some(),
            "a hot day keeps its slice: {index}"
        );
    }

    #[tokio::test]
    async fn a_day_with_a_locked_entry_is_refused_and_nothing_moves() {
        let (client, d) = app().await;
        let token = fixture(&client).await;
        let frozen = entry_on(&client, &token, DAY, 3).await;
        entry_on(&client, &token, LATER, 1).await;

        let data = data_root(&d);
        let store = Store::open(&data).expect("store");
        // Snapshot after opening the store, so the comparison isolates the pass.
        let before = snapshot(&data);
        let locks = LockOne(Some(Uuid::parse_str(&frozen).expect("uuid")));
        let report = store
            .archive_entries_before(cutoff(), false, &locks)
            .expect("the pass runs");

        assert!(report.days.is_empty(), "{:?}", report.days);
        assert!(report.manifest.is_none(), "nothing moved, so no manifest");
        assert_eq!(report.documents, 0);
        assert_eq!(report.bytes, 0);
        assert_eq!(report.skipped.len(), 1, "{:?}", report.skipped);
        assert_eq!(report.skipped[0].day, DAY);
        assert_eq!(report.skipped[0].entries, 1);
        assert_eq!(report.skipped[0].reason, ArchiveRefusal::Locked);
        assert_eq!(report.skipped_entries, 1);

        // A refused pass writes nothing at all: not the day, not the index, not
        // even the cold tree.
        assert_eq!(snapshot(&data), before);
        assert!(
            !data.join("archive").exists(),
            "a refused pass creates no cold tree"
        );
    }

    #[tokio::test]
    async fn a_day_on_a_non_draft_invoice_is_refused_and_a_draft_alone_is_not() {
        let (client, d) = app().await;
        let token = fixture(&client).await;
        entry_on(&client, &token, DAY, 3).await;

        let data = data_root(&d);
        let store = Store::open(&data).expect("store");
        let (status, draft) = json_req(
            &client,
            "POST",
            "/api/invoices",
            Some(json!({"customer_id": token, "from": "2026-08-01", "to": "2026-08-07"})),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{draft}");
        assert_eq!(draft["status"], "draft");

        // `already_invoiced` excludes only *non-draft* invoices (#248), so a draft
        // referencing the day does not refuse it — and a dry run still writes
        // nothing.
        let before = snapshot(&data);
        let planned = store
            .archive_entries_before(cutoff(), true, &NoLocks)
            .expect("dry run");
        assert!(planned.skipped.is_empty(), "{:?}", planned.skipped);
        assert_eq!(planned.days.len(), 1, "{:?}", planned.days);
        assert_eq!(planned.days[0].day, DAY);
        assert_eq!(snapshot(&data), before, "a dry run writes nothing");

        // Issue the invoice: the day is now referenced by a non-draft document,
        // which is the permanent refusal (settling releases the edit *lock*, never
        // the billing evidence).
        let id = draft["id"].as_str().expect("invoice id");
        let (status, issued) =
            json_req(&client, "POST", &format!("/api/invoices/{id}/issue"), None).await;
        assert_eq!(status, StatusCode::OK, "{issued}");
        assert_eq!(issued["status"], "issued");

        let before = snapshot(&data);
        let report = store
            .archive_entries_before(cutoff(), false, &NoLocks)
            .expect("the pass runs");
        assert!(report.days.is_empty(), "{:?}", report.days);
        assert!(report.manifest.is_none());
        assert_eq!(report.skipped.len(), 1, "{:?}", report.skipped);
        assert_eq!(report.skipped[0].day, DAY);
        assert_eq!(report.skipped[0].reason, ArchiveRefusal::AlreadyInvoiced);
        assert_eq!(report.skipped_entries, 1);
        assert_eq!(snapshot(&data), before);
    }

    #[tokio::test]
    async fn a_dry_run_reports_the_plan_and_writes_nothing_at_all() {
        let (client, d) = app().await;
        let token = fixture(&client).await;
        let frozen = entry_on(&client, &token, DAY, 3).await;
        entry_on(&client, &token, "2026-08-05", 2).await;

        let data = data_root(&d);
        let store = Store::open(&data).expect("store");
        let before = snapshot(&data);

        // A frozen day is reported, a clean one is planned, and not one byte is
        // written: no cold tree, no manifest, no index rewrite, no temp file.
        let locks = LockOne(Some(Uuid::parse_str(&frozen).expect("uuid")));
        let report = store
            .archive_entries_before(cutoff(), true, &locks)
            .expect("dry run");

        assert!(report.dry_run);
        assert_eq!(report.cutoff, CUTOFF);
        assert_eq!(report.days.len(), 1, "{:?}", report.days);
        assert_eq!(report.days[0].day, "2026-08-05");
        assert_eq!(report.days[0].documents, 1);
        assert!(report.days[0].bytes > 0);
        assert_eq!(report.documents, 1);
        assert_eq!(report.skipped.len(), 1, "{:?}", report.skipped);
        assert_eq!(report.skipped[0].reason, ArchiveRefusal::Locked);
        assert_eq!(report.skipped_entries, 1);
        assert!(report.manifest.is_none());

        assert_eq!(snapshot(&data), before, "a dry run writes nothing at all");
        assert!(
            !data.join("archive").exists(),
            "a dry run creates no cold tree"
        );
    }

    #[tokio::test]
    async fn only_days_strictly_before_the_cutoff_are_candidates() {
        let (client, d) = app().await;
        let token = fixture(&client).await;
        entry_on(&client, &token, "2026-08-31", 1).await;
        entry_on(&client, &token, CUTOFF, 1).await;
        entry_on(&client, &token, LATER, 1).await;

        let data = data_root(&d);
        let store = Store::open(&data).expect("store");
        let report = store
            .archive_entries_before(cutoff(), false, &NoLocks)
            .expect("the pass runs");

        assert_eq!(report.days.len(), 1, "{:?}", report.days);
        assert_eq!(report.days[0].day, "2026-08-31");
        assert!(
            report.skipped.is_empty(),
            "a day on or after the cutoff is out of scope, not refused: {:?}",
            report.skipped
        );
        assert!(!entry_dir(&data, "2026-08-31").exists());
        assert!(
            entry_dir(&data, CUTOFF).is_dir(),
            "the cutoff day itself is untouched"
        );
        assert!(entry_dir(&data, LATER).is_dir());
    }

    #[tokio::test]
    async fn an_existing_cold_day_is_refused_rather_than_overwritten() {
        let (client, d) = app().await;
        let token = fixture(&client).await;
        entry_on(&client, &token, DAY, 3).await;

        let data = data_root(&d);
        // A previous run already published this day (or an operator put it there).
        let cold = data.join("archive/entries/2026-08").join(DAY);
        std::fs::create_dir_all(&cold).expect("cold day");
        std::fs::write(cold.join("keep-me.json"), b"{\"kept\":true}").expect("cold document");

        let store = Store::open(&data).expect("store");
        let before = snapshot(&data);
        let report = store
            .archive_entries_before(cutoff(), false, &NoLocks)
            .expect("the pass runs");

        assert!(report.days.is_empty(), "{:?}", report.days);
        assert!(report.manifest.is_none());
        assert_eq!(report.skipped.len(), 1, "{:?}", report.skipped);
        assert_eq!(report.skipped[0].reason, ArchiveRefusal::ColdDayExists);
        assert_eq!(report.skipped_entries, 1);
        assert_eq!(
            snapshot(&data),
            before,
            "neither the cold copy nor the hot day is touched"
        );
        assert!(entry_dir(&data, DAY).is_dir());
    }

    #[tokio::test]
    async fn a_day_that_is_not_a_well_formed_day_folder_is_refused() {
        let (client, d) = app().await;
        let token = fixture(&client).await;
        entry_on(&client, &token, DAY, 3).await;

        let data = data_root(&d);
        std::fs::write(entry_dir(&data, DAY).join("readme.txt"), b"not an entry")
            .expect("foreign document");
        let store = Store::open(&data).expect("store");
        let before = snapshot(&data);
        let report = store
            .archive_entries_before(cutoff(), false, &NoLocks)
            .expect("the pass runs");

        assert!(report.days.is_empty(), "{:?}", report.days);
        assert_eq!(report.skipped.len(), 1, "{:?}", report.skipped);
        assert_eq!(report.skipped[0].reason, ArchiveRefusal::Unrecognised);
        assert_eq!(
            report.skipped[0].entries, 2,
            "the entry and the foreign document"
        );
        assert_eq!(snapshot(&data), before);

        // A directory under `entries/` that is not a day folder at all is reported
        // too, and never moved.
        std::fs::create_dir_all(data.join("entries/misc")).expect("stray directory");
        let report = store
            .archive_entries_before(cutoff(), false, &NoLocks)
            .expect("the pass runs");
        assert!(report.days.is_empty(), "{:?}", report.days);
        assert_eq!(report.skipped.len(), 2, "{:?}", report.skipped);
        assert!(
            report
                .skipped
                .iter()
                .all(|s| s.reason == ArchiveRefusal::Unrecognised),
            "{:?}",
            report.skipped
        );
        assert!(data.join("entries/misc").is_dir());
    }

    #[tokio::test]
    async fn an_unverifiable_lock_state_refuses_every_candidate_day() {
        let (client, d) = app().await;
        let token = fixture(&client).await;
        entry_on(&client, &token, DAY, 3).await;
        entry_on(&client, &token, "2026-08-05", 2).await;

        let data = data_root(&d);
        let store = Store::open(&data).expect("store");
        let before = snapshot(&data);
        let report = store
            .archive_entries_before(cutoff(), false, &Unverifiable)
            .expect("the pass runs");

        assert!(report.days.is_empty(), "{:?}", report.days);
        assert!(report.manifest.is_none());
        assert_eq!(report.skipped.len(), 2, "{:?}", report.skipped);
        assert!(
            report
                .skipped
                .iter()
                .all(|s| s.reason == ArchiveRefusal::LockUnavailable),
            "fail closed (#185): {:?}",
            report.skipped
        );
        assert_eq!(report.skipped_entries, 2);
        assert_eq!(
            snapshot(&data),
            before,
            "an unverifiable pass writes nothing"
        );
    }

    #[tokio::test]
    async fn a_second_pass_moves_nothing_and_leaves_the_cold_tree_alone() {
        let (client, d) = app().await;
        let token = fixture(&client).await;
        entry_on(&client, &token, DAY, 3).await;

        let data = data_root(&d);
        let store = Store::open(&data).expect("store");
        let first = store
            .archive_entries_before(cutoff(), false, &NoLocks)
            .expect("first pass");
        assert_eq!(first.days.len(), 1, "{:?}", first.days);
        let cold = snapshot(&data.join("archive"));

        let before = snapshot(&data);
        let again = store
            .archive_entries_before(cutoff(), false, &NoLocks)
            .expect("second pass");

        assert!(again.days.is_empty(), "{:?}", again.days);
        assert!(again.skipped.is_empty(), "{:?}", again.skipped);
        assert_eq!(again.documents, 0);
        assert_eq!(again.bytes, 0);
        assert_eq!(again.skipped_entries, 0);
        assert!(
            again.manifest.is_none(),
            "nothing moved, so no second manifest"
        );
        assert_eq!(snapshot(&data), before, "a re-run is idempotent");
        assert_eq!(
            snapshot(&data.join("archive")),
            cold,
            "the cold tree is untouched by a re-run"
        );
        let manifests = std::fs::read_dir(data.join("archive/entries"))
            .expect("cold root")
            .filter_map(|entry| entry.ok())
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".manifest.")
            })
            .count();
        assert_eq!(manifests, 1, "exactly the manifest the first pass wrote");
    }
}

/// #350: the cold-tree *read story* — ADR-003 precondition 2. A relocation moves
/// bytes and changes no number any caller or user can see.
///
/// WHAT THESE PIN
/// * a relocated day is still a day: it reads by date, by range, by id, from the
///   lifetime scan and from every `list_all_entries`-backed guard, and nothing
///   the user sees changes because a folder moved;
/// * the merge is by entry id with the **hot** copy winning, so a day a crash
///   left in both trees is never counted or returned twice;
/// * the #335 trust rule compares against the **merged** day, and its heal files
///   hot ids only — otherwise a member's own archived week reads as zero hours,
///   or the index re-gains a key naming a day that is not on disk (#349);
/// * a cold entry stays writable and deletable: an edit rehydrates it (exactly
///   one document, hot) and a delete works, including the second one that must
///   404 rather than silently succeed;
/// * the cold id index is a hint: absent, truncated or lying, the read is still
///   the right one, and no read ever writes it;
/// * the `MAX_DOCS` cap is one budget over the union, not one per tree.
mod cold_tree_reads {
    use std::collections::HashSet;
    use std::path::{Path, PathBuf};

    use axum::http::StatusCode;
    use serde_json::{Value, json};
    use tucano_time::domain::Entry;
    use tucano_time::lock::NoLocks;
    use tucano_time::store::{ArchiveRefusal, Store};
    use uuid::Uuid;

    use crate::support::{
        Client, app, entry_body, json_req, member_cookie, new_customer, new_project, raw,
    };

    /// One day old enough to be a candidate, one newer than the cutoff, and the
    /// cutoff itself (#349's fixture days).
    const DAY: &str = "2026-08-03";
    const LATER: &str = "2026-10-09";
    const CUTOFF: &str = "2026-09-01";
    const MEMBER_EMAIL: &str = "dana@test.local";
    const MEMBER_PW: &str = "danapass123";

    fn cutoff() -> chrono::NaiveDate {
        chrono::NaiveDate::parse_from_str(CUTOFF, "%Y-%m-%d").expect("cutoff")
    }

    fn day() -> chrono::NaiveDate {
        chrono::NaiveDate::parse_from_str(DAY, "%Y-%m-%d").expect("day")
    }

    fn later() -> chrono::NaiveDate {
        chrono::NaiveDate::parse_from_str(LATER, "%Y-%m-%d").expect("later")
    }

    /// The data root below the temp dir the harness hands out.
    fn data_root(dir: &tempfile::TempDir) -> PathBuf {
        dir.path().join("data")
    }

    fn hot_day(data: &Path, date: &str) -> PathBuf {
        data.join("entries").join(date)
    }

    /// `archive/entries/<YYYY-MM>/<YYYY-MM-DD>/` — the documented cold layout,
    /// spelled here so the test does not borrow the store's own builder.
    fn cold_day(data: &Path, date: &str) -> PathBuf {
        data.join("archive/entries").join(&date[..7]).join(date)
    }

    /// The #350 cold id index: a dot file in the cold root.
    fn cold_index_path(data: &Path) -> PathBuf {
        data.join("archive/entries/.idx.ids.json")
    }

    /// The #335 per-user index.
    fn user_index_path(data: &Path) -> PathBuf {
        data.join("entries").join(".idx.users.json")
    }

    fn store_at(data: &Path) -> Store {
        Store::open(data).expect("store")
    }

    /// Run the #349 pass over the temp data dir and return how many days moved.
    /// A refused day fails the call: these tests archive clean days only.
    fn archive(data: &Path) -> usize {
        let report = store_at(data)
            .archive_entries_before(cutoff(), false, &NoLocks)
            .expect("the pass runs");
        assert!(report.skipped.is_empty(), "{:?}", report.skipped);
        report.days.len()
    }

    /// A customer with project `P1`, as the customer token payloads need.
    async fn fixture(client: &Client) -> String {
        let customer = new_customer(client, "ACME", "EUR", 6000).await;
        let token = customer["id"].as_str().expect("customer id").to_string();
        new_project(client, &token, "P1", json!({})).await;
        token
    }

    /// One entry on `date`, created by `cookie`'s session (the harness's admin
    /// when `None`) — the only way to give a member a row of their own.
    async fn entry_on(
        client: &Client,
        cookie: Option<&str>,
        token: &str,
        date: &str,
        hours: u32,
        task: Option<&str>,
    ) -> String {
        let mut body = entry_body(token, "P1", json!(hours), date);
        if let Some(code) = task {
            body["task_code"] = json!(code);
        }
        let (status, created) = match cookie {
            Some(c) => {
                let (s, v, _) =
                    raw(&client.router, "POST", "/api/entries", Some(body), Some(c)).await;
                (s, v)
            }
            None => json_req(client, "POST", "/api/entries", Some(body)).await,
        };
        assert_eq!(status, StatusCode::CREATED, "create entry: {created}");
        created["id"].as_str().expect("entry id").to_string()
    }

    /// `GET /api/entries?...` as `cookie`'s session when given, as the harness's
    /// admin otherwise. The status must be 200: these tests read valid queries.
    async fn list_entries(client: &Client, cookie: Option<&str>, query: &str) -> Value {
        let uri = format!("/api/entries?{query}");
        let (status, body) = match cookie {
            Some(c) => {
                let (s, v, _) = raw(&client.router, "GET", &uri, None, Some(c)).await;
                (s, v)
            }
            None => json_req(client, "GET", &uri, None).await,
        };
        assert_eq!(status, StatusCode::OK, "{body}");
        body
    }

    fn listed_ids(body: &Value) -> HashSet<String> {
        body["entries"]
            .as_array()
            .expect("an entries array")
            .iter()
            .map(|e| e["id"].as_str().expect("an entry id").to_string())
            .collect()
    }

    /// The ids of a store-level day read, for the merge assertions.
    fn doc_ids(docs: &[Entry]) -> Vec<String> {
        docs.iter().map(|e| e.id.to_string()).collect()
    }

    /// A day read, by date and by a one-day range, is unchanged by a relocation.
    #[tokio::test]
    async fn an_archived_day_still_reads_by_date_and_by_range() {
        let (client, d) = app().await;
        let token = fixture(&client).await;
        let first = entry_on(&client, None, &token, DAY, 3, None).await;
        let second = entry_on(&client, None, &token, DAY, 4, None).await;
        let hot = entry_on(&client, None, &token, LATER, 1, None).await;

        let data = data_root(&d);
        assert_eq!(archive(&data), 1, "exactly the one candidate day moved");

        // By date.
        let listed = list_entries(&client, None, &format!("date={DAY}")).await;
        assert_eq!(
            listed_ids(&listed),
            HashSet::from([first.clone(), second.clone()]),
            "{listed}"
        );
        // The hours the relocation must not change, whatever the page order.
        let mut hours: Vec<u64> = listed["entries"]
            .as_array()
            .expect("entries")
            .iter()
            .map(|e| e["hours"].as_u64().expect("hours"))
            .collect();
        hours.sort();
        assert_eq!(hours, vec![3, 4], "{listed}");

        // By range: the archived day and the hot day together, in one answer.
        let ranged = list_entries(&client, None, &format!("from={DAY}&to={LATER}")).await;
        assert_eq!(
            listed_ids(&ranged),
            HashSet::from([first.clone(), second.clone(), hot.clone()]),
            "{ranged}"
        );

        // By id, and at the store level too: the numbers a relocation must not
        // touch are the same hundredths they were before the move.
        let store = store_at(&data);
        let docs = store.list_by_date(day()).expect("day read");
        assert_eq!(docs.len(), 2);
        assert_eq!(docs.iter().map(|e| e.hours.0).sum::<u32>(), 700);
        assert_eq!(
            store.list_range(day(), day()).expect("range read").len(),
            2,
            "a one-day range is the same day"
        );

        // Negative: a day with nothing in either tree is empty, not an error; and
        // a query carrying neither a date nor a range is refused.
        let empty = list_entries(&client, None, "date=2026-08-04").await;
        assert!(listed_ids(&empty).is_empty(), "{empty}");
        let (bad, body) = json_req(&client, "GET", "/api/entries", None).await;
        assert_eq!(bad, StatusCode::BAD_REQUEST, "{body}");
    }

    /// A point read resolves the cold copy, and an unknown id still misses.
    #[tokio::test]
    async fn a_point_read_finds_an_archived_entry_and_an_unknown_id_still_misses() {
        let (client, d) = app().await;
        let token = fixture(&client).await;
        let archived = entry_on(&client, None, &token, DAY, 4, None).await;

        let data = data_root(&d);
        assert_eq!(archive(&data), 1);
        let id = Uuid::parse_str(&archived).expect("uuid");

        let store = store_at(&data);
        let found = store
            .get_entry(id)
            .expect("point read")
            .expect("the cold copy answers");
        assert_eq!(found.id, id);
        assert_eq!(found.hours.0, 400);
        assert_eq!(found.date, day());
        assert!(
            store
                .get_entry(Uuid::new_v4())
                .expect("point read")
                .is_none(),
            "an id in neither tree is still a miss"
        );

        // The HTTP surface answers the same way, and 404s the same way.
        let (ok, body) = json_req(&client, "GET", &format!("/api/entries/{archived}"), None).await;
        assert_eq!(ok, StatusCode::OK, "{body}");
        assert_eq!(body["id"], archived.as_str());
        assert_eq!(body["hours"], json!(4));
        let (missing, _) = json_req(
            &client,
            "GET",
            &format!("/api/entries/{}", Uuid::new_v4()),
            None,
        )
        .await;
        assert_eq!(missing, StatusCode::NOT_FOUND);
    }

    /// The lifetime scan is hot ∪ cold, and counts a document that is in both
    /// trees exactly once.
    #[tokio::test]
    async fn the_lifetime_scan_counts_a_relocated_day_exactly_once() {
        let (client, d) = app().await;
        let token = fixture(&client).await;
        let first = entry_on(&client, None, &token, DAY, 3, None).await;
        let second = entry_on(&client, None, &token, DAY, 4, None).await;
        entry_on(&client, None, &token, LATER, 1, None).await;

        let data = data_root(&d);
        assert_eq!(archive(&data), 1);
        let store = store_at(&data);
        let all = store.list_all_entries().expect("lifetime scan");
        assert_eq!(all.len(), 3, "hot ∪ cold, once each");
        assert_eq!(
            all.iter()
                .map(|e| e.id.to_string())
                .collect::<HashSet<_>>()
                .len(),
            3,
            "no id twice"
        );

        // The crash window the #349 pass is built to survive, reproduced by hand:
        // the cold copy is complete and the hot day has not been removed yet.
        let hot_dir = hot_day(&data, DAY);
        std::fs::create_dir_all(&hot_dir).expect("hot day");
        for id in [&first, &second] {
            std::fs::copy(
                cold_day(&data, DAY).join(format!("{id}.json")),
                hot_dir.join(format!("{id}.json")),
            )
            .expect("copy back");
        }
        let all = store.list_all_entries().expect("lifetime scan");
        assert_eq!(all.len(), 3, "a day in both trees is still three entries");
        assert_eq!(
            store.list_by_date(day()).expect("day read").len(),
            2,
            "nor does a day read return it twice"
        );

        // Hot wins the merge: it is the copy the pass had not finished with, so a
        // half-published day reads as the document it read as before the pass.
        let path = hot_dir.join(format!("{first}.json"));
        let mut doc: Value =
            serde_json::from_slice(&std::fs::read(&path).expect("hot copy")).expect("json");
        doc["hours"] = json!(9);
        std::fs::write(&path, serde_json::to_vec(&doc).expect("json")).expect("rewrite hot copy");
        let docs = store.list_by_date(day()).expect("day read");
        assert_eq!(docs.len(), 2);
        let merged = docs
            .iter()
            .find(|e| e.id.to_string() == first)
            .expect("the hot copy answers");
        assert_eq!(merged.hours.0, 900, "the hot copy wins the merge");
    }

    /// QA-05 moved the `MAX_DOCS` cap to the write path: a day whose union
    /// (hot ∪ cold) exceeds the cap is still listed in full — reading never
    /// fails and never truncates — so a pre-existing over-cap tree stays
    /// recoverable.
    #[tokio::test]
    async fn an_over_cap_day_still_lists_its_whole_union() {
        let (client, d) = app().await;
        let token = fixture(&client).await;
        // Two rows move to the cold tree, and two more land on the same date in
        // the hot tree afterwards: one day, split between the trees.
        entry_on(&client, None, &token, DAY, 3, None).await;
        entry_on(&client, None, &token, DAY, 4, None).await;

        let data = data_root(&d);
        assert_eq!(archive(&data), 1);
        let cold = cold_day(&data, DAY).read_dir().expect("cold day").count();
        assert_eq!(cold, 2, "the whole day moved");
        entry_on(&client, None, &token, DAY, 1, None).await;
        entry_on(&client, None, &token, DAY, 2, None).await;

        // Each tree alone is inside a cap of three; their union is not. The
        // read lists all four documents anyway (the cap is a write guard).
        let capped = store_at(&data).with_max_docs(3);
        assert_eq!(
            capped
                .list_by_date(day())
                .expect("over-cap day read lists")
                .len(),
            4
        );
        assert_eq!(
            capped
                .list_all_entries()
                .expect("over-cap lifetime scan lists")
                .len(),
            4
        );
    }

    /// Budget totals and every `list_all_entries`-backed deletion guard still see
    /// archived history — and stop seeing it when it is deleted.
    #[tokio::test]
    async fn budget_totals_and_the_deletion_guards_still_see_archived_history() {
        let (client, d) = app().await;
        let customer = new_customer(&client, "ACME", "EUR", 6000).await;
        let cid = customer["id"].as_str().expect("customer id").to_string();
        new_project(&client, &cid, "P1", json!({"budget_hours": 10})).await;
        let (s_task, task) = json_req(
            &client,
            "POST",
            &format!("/api/customers/{cid}/projects/P1/tasks"),
            Some(json!({"code": "T-9"})),
        )
        .await;
        assert_eq!(s_task, StatusCode::CREATED, "{task}");

        // A member's own row, carrying the task, so `delete_user` and
        // `has_entries_for_task` both have archived history to answer on.
        let dana = member_cookie(&client, MEMBER_EMAIL, MEMBER_PW).await;
        let member_row = entry_on(&client, Some(&dana), &cid, DAY, 4, Some("T-9")).await;
        entry_on(&client, None, &cid, LATER, 3, None).await;
        let (_, users) = json_req(&client, "GET", "/api/users", None).await;
        let member_id = users["users"]
            .as_array()
            .expect("users")
            .iter()
            .find(|u| u["email"] == MEMBER_EMAIL)
            .expect("the member")["id"]
            .as_str()
            .expect("member id")
            .to_string();

        let data = data_root(&d);
        assert_eq!(archive(&data), 1);

        // The budget report is the lifetime scan behind the merge: the archived
        // member row is 4 of the 7 hours burned.
        let (s_budget, budgets) = json_req(&client, "GET", "/api/reports/budgets", None).await;
        assert_eq!(s_budget, StatusCode::OK, "{budgets}");
        let row = budgets["rows"]
            .as_array()
            .expect("budget rows")
            .iter()
            .find(|r| r["customer"] == "ACME")
            .expect("the budgeted project");
        assert_eq!(
            row["burn_hours"].as_f64(),
            Some(7.0),
            "archived hours are lifetime hours: {row}"
        );

        // Every guard behind `list_all_entries` refuses while the archived rows
        // exist — so the refusals below are proof the cold tree was scanned.
        for uri in [
            format!("/api/users/{member_id}"),
            format!("/api/customers/{cid}"),
            format!("/api/customers/{cid}/projects/P1"),
            format!("/api/customers/{cid}/projects/P1/tasks/T-9"),
        ] {
            let (status, body) = json_req(&client, "DELETE", &uri, None).await;
            assert_eq!(status, StatusCode::CONFLICT, "{uri}: {body}");
        }

        // Delete the archived row: the task and the member are no longer refused,
        // while the customer and the project still are (the hot row remains).
        let (s_del, body) = json_req(
            &client,
            "DELETE",
            &format!("/api/entries/{member_row}"),
            None,
        )
        .await;
        assert_eq!(s_del, StatusCode::NO_CONTENT, "{body}");
        assert!(
            !cold_day(&data, DAY)
                .join(format!("{member_row}.json"))
                .exists(),
            "the cold copy goes with the delete"
        );
        for uri in [
            format!("/api/customers/{cid}/projects/P1/tasks/T-9"),
            format!("/api/users/{member_id}"),
        ] {
            let (status, body) = json_req(&client, "DELETE", &uri, None).await;
            assert!(
                status.is_success(),
                "the archived row was what refused this: {uri}: {status} {body}"
            );
        }
        let (still, body) =
            json_req(&client, "DELETE", &format!("/api/customers/{cid}"), None).await;
        assert_eq!(still, StatusCode::CONFLICT, "{body}");
    }

    /// The #335 trust rule must compare against the **merged** day, or a member's
    /// own archived week reads as zero hours with no error.
    #[tokio::test]
    async fn a_members_own_archived_day_is_not_emptied_by_the_trust_rule() {
        let (client, d) = app().await;
        let token = fixture(&client).await;
        let dana = member_cookie(&client, MEMBER_EMAIL, MEMBER_PW).await;
        let mine = entry_on(&client, Some(&dana), &token, DAY, 4, None).await;
        let theirs = entry_on(&client, None, &token, DAY, 3, None).await;

        let data = data_root(&d);
        assert_eq!(archive(&data), 1, "both owners' rows moved with the day");

        // #349 pruned the day from the per-user index (it may only name days that
        // are on disk), so the index's id set for it is empty *and* the hot day
        // folder's is empty: comparing against the hot folder alone would
        // "validate" an empty read.
        let index: Value =
            serde_json::from_slice(&std::fs::read(user_index_path(&data)).expect("index"))
                .expect("json");
        assert!(index["days"].get(DAY).is_none(), "{index}");

        let listed = list_entries(&client, Some(&dana), &format!("date={DAY}")).await;
        assert_eq!(
            listed_ids(&listed),
            HashSet::from([mine.clone()]),
            "{listed}"
        );
        assert_eq!(listed["entries"][0]["hours"], json!(4), "{listed}");

        // A sibling's archived row is never a member's to see (#51) — by day or by
        // id, and a 404 rather than a 403 so existence is not leaked.
        let (s_sibling, _, _) = raw(
            &client.router,
            "GET",
            &format!("/api/entries/{theirs}"),
            None,
            Some(&dana),
        )
        .await;
        assert_eq!(s_sibling, StatusCode::NOT_FOUND);

        // The heal filed hot ids only: the index gained no key naming the cold day,
        // which is exactly the #349 invariant a merged heal would regress.
        let index: Value =
            serde_json::from_slice(&std::fs::read(user_index_path(&data)).expect("index"))
                .expect("json");
        assert!(
            index["days"].get(DAY).is_none(),
            "the heal must not name a cold day: {index}"
        );

        // Negative: the same member reads a day with nothing in either tree.
        let empty = list_entries(&client, Some(&dana), "date=2026-08-05").await;
        assert!(listed_ids(&empty).is_empty(), "{empty}");
    }

    /// A cold entry is edited, moved out of a cold day and deleted: each write
    /// rehydrates it and leaves exactly one document behind.
    #[tokio::test]
    async fn editing_moving_or_deleting_a_cold_entry_rehydrates_it() {
        let (client, d) = app().await;
        let token = fixture(&client).await;
        let first = entry_on(&client, None, &token, DAY, 3, None).await;
        let second = entry_on(&client, None, &token, DAY, 4, None).await;

        let data = data_root(&d);
        assert_eq!(archive(&data), 1);

        // An edit rehydrates: the hot document exists, the cold copy is gone, and
        // the sibling that was not touched stays cold.
        let (s_edit, edited) = json_req(
            &client,
            "PUT",
            &format!("/api/entries/{first}"),
            Some(entry_body(&token, "P1", json!(6), DAY)),
        )
        .await;
        assert_eq!(s_edit, StatusCode::OK, "{edited}");
        assert!(hot_day(&data, DAY).join(format!("{first}.json")).is_file());
        assert!(
            !cold_day(&data, DAY).join(format!("{first}.json")).exists(),
            "the edit removed the cold copy"
        );
        assert!(
            cold_day(&data, DAY)
                .join(format!("{second}.json"))
                .is_file(),
            "the untouched sibling stays cold"
        );
        let docs = store_at(&data).list_by_date(day()).expect("day read");
        assert_eq!(doc_ids(&docs).len(), 2, "an edit never duplicates a day");

        // A move **out of** a cold day strands nothing: the cold copy of the moved
        // id is gone, and each day holds one document.
        let (s_move, moved) = json_req(
            &client,
            "PUT",
            &format!("/api/entries/{second}"),
            Some(entry_body(&token, "P1", json!(5), LATER)),
        )
        .await;
        assert_eq!(s_move, StatusCode::OK, "{moved}");
        assert_eq!(moved["date"], LATER);
        assert!(
            !cold_day(&data, DAY).join(format!("{second}.json")).exists(),
            "the move left nothing cold behind"
        );
        let store = store_at(&data);
        assert_eq!(
            doc_ids(&store.list_by_date(day()).expect("day read")),
            vec![first.clone()]
        );
        assert_eq!(
            doc_ids(&store.list_by_date(later()).expect("day read")),
            vec![second.clone()]
        );

        // A delete of a cold entry works, and the second one is a 404 — the check
        // spans both trees, so a successful `get_entry` is never a 404 here.
        let third = entry_on(&client, None, &token, "2026-08-06", 2, None).await;
        let report = store
            .archive_entries_before(cutoff(), false, &NoLocks)
            .expect("second pass");
        assert!(
            report.days.iter().any(|d| d.day == "2026-08-06"),
            "{:?}",
            report.days
        );
        assert!(
            cold_day(&data, "2026-08-06")
                .join(format!("{third}.json"))
                .is_file()
        );
        let (s_del, body) =
            json_req(&client, "DELETE", &format!("/api/entries/{third}"), None).await;
        assert_eq!(s_del, StatusCode::NO_CONTENT, "{body}");
        assert!(
            !cold_day(&data, "2026-08-06")
                .join(format!("{third}.json"))
                .exists()
        );
        let (s_again, body) =
            json_req(&client, "DELETE", &format!("/api/entries/{third}"), None).await;
        assert_eq!(s_again, StatusCode::NOT_FOUND, "{body}");
    }

    /// A half-published day — a day partly back in the hot tree, a stale index and
    /// a leftover staging folder — reads without error and without duplicates.
    #[tokio::test]
    async fn a_half_published_day_reads_without_error_or_duplicates() {
        let (client, d) = app().await;
        let token = fixture(&client).await;
        let first = entry_on(&client, None, &token, DAY, 3, None).await;
        let second = entry_on(&client, None, &token, DAY, 4, None).await;

        let data = data_root(&d);
        assert_eq!(archive(&data), 1);

        // The crash window, half-resolved: the cold copies of both documents are
        // complete, but only one of them is back in the hot tree — so the day is
        // now genuinely split between the trees. The index still does not name the
        // day, and the pass's hidden staging folder is still there.
        let hot_dir = hot_day(&data, DAY);
        std::fs::create_dir_all(&hot_dir).expect("hot day");
        std::fs::copy(
            cold_day(&data, DAY).join(format!("{first}.json")),
            hot_dir.join(format!("{first}.json")),
        )
        .expect("copy back");
        let mut doc: Value = serde_json::from_slice(
            &std::fs::read(hot_dir.join(format!("{first}.json"))).expect("hot copy"),
        )
        .expect("json");
        doc["hours"] = json!(9);
        std::fs::write(
            hot_dir.join(format!("{first}.json")),
            serde_json::to_vec(&doc).expect("json"),
        )
        .expect("rewrite the hot copy");
        let staging = data
            .join("archive/entries/2026-08")
            .join(format!(".{DAY}.staging"));
        std::fs::create_dir_all(&staging).expect("staging folder");
        std::fs::write(staging.join("junk.json"), b"{\"junk\":true}").expect("junk document");

        // The day is the union, and each id resolves to the copy that wins: `first`
        // to the hot one the pass had not finished with, `second` to the cold one.
        let store = store_at(&data);
        let docs = store.list_by_date(day()).expect("day read");
        assert_eq!(doc_ids(&docs).len(), 2, "a split day is one day: {docs:?}");
        assert_eq!(
            docs.iter()
                .find(|e| e.id.to_string() == first)
                .expect("the hot copy answers")
                .hours
                .0,
            900,
            "the hot copy wins the merge"
        );
        assert_eq!(
            docs.iter()
                .find(|e| e.id.to_string() == second)
                .expect("the cold copy answers")
                .hours
                .0,
            400,
            "a cold-only row is in the day too"
        );
        assert_eq!(
            store.list_all_entries().expect("lifetime scan").len(),
            2,
            "the union counts each document once"
        );
        assert!(
            store
                .get_entry(Uuid::parse_str(&second).expect("uuid"))
                .expect("point read")
                .is_some()
        );
        assert_eq!(
            store.list_range(day(), later()).expect("range read").len(),
            2
        );

        let listed = list_entries(&client, None, &format!("date={DAY}")).await;
        assert_eq!(
            listed_ids(&listed),
            HashSet::from([first.clone(), second.clone()]),
            "{listed}"
        );

        // Publishing the day again is refused rather than merged: the staging
        // folder and the dot files are not day folders, and the cold day it would
        // overwrite is.
        let report = store
            .archive_entries_before(cutoff(), false, &NoLocks)
            .expect("second pass");
        assert!(report.days.is_empty(), "{:?}", report.days);
        assert!(
            report
                .skipped
                .iter()
                .any(|s| s.day == DAY && s.reason == ArchiveRefusal::ColdDayExists),
            "{:?}",
            report.skipped
        );
    }

    /// The cold id index is written by the pass and never trusted blindly: absent,
    /// truncated or lying, the read is still the right one — and no read writes it.
    #[tokio::test]
    async fn the_cold_index_is_never_the_source_of_truth() {
        let (client, d) = app().await;
        let token = fixture(&client).await;
        let first = entry_on(&client, None, &token, DAY, 3, None).await;
        let second = entry_on(&client, None, &token, DAY, 4, None).await;

        let data = data_root(&d);
        assert_eq!(archive(&data), 1);
        let written: Value = serde_json::from_slice(
            &std::fs::read(cold_index_path(&data)).expect("the pass writes the cold id index"),
        )
        .expect("json");
        assert_eq!(
            written[DAY].as_array().expect("the day slice").len(),
            2,
            "{written}"
        );

        // Absent: the walk answers, and a read never puts the file back.
        std::fs::remove_file(cold_index_path(&data)).expect("remove the index");
        let store = store_at(&data);
        assert!(
            store
                .get_entry(Uuid::parse_str(&second).expect("uuid"))
                .expect("point read")
                .is_some(),
            "an absent index costs a walk, not the answer"
        );
        assert!(
            !cold_index_path(&data).exists(),
            "no read writes the cold index"
        );

        // Truncated: the same.
        std::fs::write(cold_index_path(&data), b"{\"2026-08-03\": [").expect("truncated index");
        assert!(
            store
                .get_entry(Uuid::parse_str(&first).expect("uuid"))
                .expect("point read")
                .is_some(),
            "a truncated index costs a walk, not the answer"
        );

        // Lying: a slice that omits an id would hide it if it were believed, and a
        // date the cold tree does not have is refused as a hint.
        std::fs::write(
            cold_index_path(&data),
            format!("{{\"{DAY}\": [\"{first}\"]}}").into_bytes(),
        )
        .expect("lying index");
        assert!(
            store
                .get_entry(Uuid::parse_str(&second).expect("uuid"))
                .expect("point read")
                .is_some(),
            "a lying index may not hide an entry"
        );
        std::fs::write(
            cold_index_path(&data),
            format!("{{\"2026-01-01\": [\"{second}\"]}}").into_bytes(),
        )
        .expect("lying index");
        assert!(
            store
                .get_entry(Uuid::parse_str(&second).expect("uuid"))
                .expect("point read")
                .is_some(),
            "a hint that names a day the cold tree does not have is not a day"
        );

        // The day read never consults the index at all, and the union is intact.
        assert_eq!(
            doc_ids(&store.list_by_date(day()).expect("day read")).len(),
            2
        );
    }
}

// ------------------------------------------------------------ QA-05: the cap --

/// QA-05: with a low `MAX_DOCS`, the write past the cap is a 409
/// `collection_full` and persists nothing; the collection still lists.
#[tokio::test]
async fn the_document_cap_refuses_a_write_past_it_and_persists_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = tucano_time::store::Store::open(dir.path().join("data"))
        .expect("store")
        .with_max_docs(3);
    let client = Client::new(store, None).await;

    for name in ["A", "B", "C"] {
        new_customer(&client, name, "EUR", 6000).await;
    }
    let (s_over, body) = json_req(
        &client,
        "POST",
        "/api/customers",
        Some(json!({"name":"D","currency":"EUR","default_rate_minor":6000})),
    )
    .await;
    assert_eq!(s_over, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], "collection_full", "{body}");
    // Nothing was written, and the listing still works (never a 500).
    let (s_list, listed) = json_req(&client, "GET", "/api/customers", None).await;
    assert_eq!(s_list, StatusCode::OK, "{listed}");
    assert_eq!(listed["customers"].as_array().unwrap().len(), 3);
}

/// QA-05: a pre-seeded over-cap collection (an operator restored it, or an
/// older build wrote past the cap) still lists in full — reading never fails.
#[tokio::test]
async fn a_pre_seeded_over_cap_collection_still_lists_what_is_there() {
    let dir = tempfile::tempdir().expect("tempdir");
    let data = dir.path().join("data");
    let seeded = Client::new(tucano_time::store::Store::open(&data).expect("store"), None).await;
    for name in ["A", "B", "C", "D"] {
        new_customer(&seeded, name, "EUR", 6000).await;
    }

    // Reopen the same data dir under a cap of two: the directory already holds
    // four documents.
    let capped = tucano_time::store::Store::open(&data)
        .expect("store")
        .with_max_docs(2);
    let capped = Client::new(capped, None).await;
    let (s, body) = json_req(&capped, "GET", "/api/customers", None).await;
    assert_eq!(s, StatusCode::OK, "over-cap listing must not 500: {body}");
    assert_eq!(
        body["customers"].as_array().unwrap().len(),
        4,
        "the reader lists every document present"
    );
    // Categories listing (the QA repro's second symptom) is untouched too.
    let (s_cat, _) = json_req(&capped, "GET", "/api/categories", None).await;
    assert_eq!(s_cat, StatusCode::OK);
}
