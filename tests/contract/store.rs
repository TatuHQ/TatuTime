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
    // into a bogus "customer does not exist" 422. A corrupt customer doc
    // must surface as a 500 (generic, no internals leaked).
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
    assert_eq!(b["error"]["code"], "internal");
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
