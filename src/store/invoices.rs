use std::collections::HashMap;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use uuid::Uuid;

use super::io::{read_json, write_json};
use super::*;
use crate::domain::{InvoiceStatus, InvoiceTimelineEvent, PdfHint};

impl Store {
    // ------------------------------------------------------------- invoices --

    pub fn list_invoices(&self) -> Result<Vec<Invoice>, StoreError> {
        let mut out = self.list_docs::<Invoice>()?;
        out.sort_by(|a, b| {
            a.created_at
                .cmp(&b.created_at)
                .then(a.number.cmp(&b.number))
        });
        Ok(out)
    }

    pub fn get_invoice(&self, id: Uuid) -> Result<Option<Invoice>, StoreError> {
        self.get_doc::<Invoice>(&id.to_string())
    }

    /// First invoice with this number (numbers are sequential but a restored
    /// file could duplicate one; the earliest wins). Used by payment webhooks
    /// (#34), which know the invoice by its human number: the number index
    /// turns that scan into one lookup (#102).
    pub fn find_invoice_by_number(&self, number: &str) -> Result<Option<Invoice>, StoreError> {
        let idx = read_json::<HashMap<String, String>>(&self.invoice_index_path())?;
        if let Some(id) = idx
            .as_ref()
            .and_then(|m| m.get(number))
            .and_then(|s| Uuid::parse_str(s).ok())
            && let Some(invoice) = self.get_invoice(id)?
            && invoice.number == number
        {
            return Ok(Some(invoice));
        }
        // Authoritative scan (also covers an absent or drifted index).
        let mut found: Option<Invoice> = None;
        for inv in self.list_invoices()? {
            if inv.number == number && found.as_ref().is_none_or(|f| inv.created_at < f.created_at)
            {
                found = Some(inv);
            }
        }
        // A hit through the scan means the fast path missed: rebuild.
        if found.is_some() {
            self.rebuild_invoice_index()?;
        }
        Ok(found)
    }

    pub fn put_invoice(&self, invoice: &Invoice) -> Result<(), StoreError> {
        let _guard = self.write_lock()?;
        let path = self.doc_path::<Invoice>(&invoice.id.to_string());
        if !path.exists() {
            self.ensure_capacity::<Invoice>()?;
        }
        std::fs::create_dir_all(self.doc_dir::<Invoice>())?;
        write_json(&self.doc_path::<Invoice>(&invoice.id.to_string()), invoice)?;
        self.index_invoice_locked(invoice)
    }

    pub fn delete_invoice(&self, id: Uuid) -> Result<(), StoreError> {
        let _guard = self.write_lock()?;
        self.remove_doc_locked::<Invoice>(&id.to_string())?;
        // #113: the invoice JSON is the record of authority — when it goes,
        // its archived PDF goes in the same transactional write (#52 logs it).
        let pdf = self.invoice_pdf_path(&id.to_string());
        if pdf.exists() {
            std::fs::remove_file(&pdf)?;
        }
        self.unindex_invoice_locked(&id.to_string())
    }

    /// Replace a DRAFT document wholesale (#143 draft editing): identity
    /// (`id`, `number`, `created_at`) and the Draft status are enforced by
    /// the store, never trusted from the payload; anything else the caller
    /// assembled lands atomically. Issued/paid/written-off documents Conflict
    /// — the snapshot rules of #8/#113/#114 stay unbreakable.
    pub fn replace_invoice_draft(
        &self,
        id: Uuid,
        mut updated: Invoice,
    ) -> Result<Invoice, StoreError> {
        let _guard = self.write_lock()?;
        let path = self.doc_path::<Invoice>(&id.to_string());
        let Some(existing) = read_json::<Invoice>(&path)? else {
            return Err(StoreError::NotFound);
        };
        if existing.status != InvoiceStatus::Draft {
            return Err(StoreError::Conflict(
                "only a draft invoice can be edited".into(),
            ));
        }
        // #256: the caller hands the assembled candidate over by value, so the
        // store stamps identity in place instead of cloning the whole draft.
        updated.id = existing.id;
        updated.number = existing.number;
        updated.created_at = existing.created_at;
        updated.status = InvoiceStatus::Draft;
        updated.issued_at = None;
        write_json(&path, &updated)?;
        self.index_invoice_locked(&updated)?;
        Ok(updated)
    }

    // ----------------------------------------------------- invoice timeline --
    //
    // A per-invoice activity timeline (Subtask A) kept in a dot-file sidecar
    // beside the invoice document, exactly like the other side artefacts:
    // `write_json` (tmp + rename), skipped by `dir_entries` so it is never a
    // document and never counts toward `MAX_DOCS`. It is a display aid only —
    // never the source of truth — so reads are lenient (a missing or malformed
    // sidecar is an empty timeline) and writes are best-effort, with the caller
    // logging rather than failing the invoice operation that triggered them.

    /// `invoices/.timeline.<id>.json` — a dot file, so the collection reader
    /// skips it.
    fn invoice_timeline_path(&self, id: Uuid) -> PathBuf {
        self.doc_dir::<Invoice>()
            .join(format!(".timeline.{id}.json"))
    }

    /// Read one invoice's timeline, oldest first. A missing or malformed
    /// sidecar is an **empty** timeline, never an error: the timeline is a
    /// display aid and a corrupt file must not break an invoice read.
    pub fn invoice_timeline(&self, id: Uuid) -> Vec<InvoiceTimelineEvent> {
        read_json::<Vec<InvoiceTimelineEvent>>(&self.invoice_timeline_path(id))
            .ok()
            .flatten()
            .unwrap_or_default()
    }

    /// Append one event to an invoice's timeline, retaining at most
    /// [`MAX_TIMELINE_EVENTS`] (oldest dropped). Lenient on read so a malformed
    /// sidecar is replaced rather than wedging every later append.
    pub fn append_invoice_timeline(
        &self,
        id: Uuid,
        event: InvoiceTimelineEvent,
    ) -> Result<(), StoreError> {
        let _guard = self.write_lock()?;
        let path = self.invoice_timeline_path(id);
        let mut events: Vec<InvoiceTimelineEvent> =
            read_json(&path).ok().flatten().unwrap_or_default();
        events.push(event);
        if events.len() > MAX_TIMELINE_EVENTS {
            let excess = events.len() - MAX_TIMELINE_EVENTS;
            events.drain(0..excess);
        }
        std::fs::create_dir_all(self.doc_dir::<Invoice>())?;
        write_json(&path, &events)
    }

    /// Atomically assign the next invoice number and persist under one writer
    /// lock, so two processes never mint the same number (#62) and a deleted
    /// invoice never frees its number for reuse (review B3): the sequence is
    /// kept in `invoices/.seq.json`, never derived from the current count.
    pub fn create_invoice(&self, invoice: Invoice) -> Result<Invoice, StoreError> {
        let _guard = self.write_lock()?;
        self.create_invoice_inner(invoice)
    }

    pub(super) fn invoice_pdf_path(&self, id: &str) -> PathBuf {
        self.root.join("invoices").join(format!("{id}.pdf"))
    }

    /// Read the archived PDF (#50: size-capped; the path is built only from a
    /// `Uuid`, never from request text). `Ok(None)` when nothing is archived.
    pub fn invoice_pdf_bytes(&self, id: Uuid) -> Result<Option<Vec<u8>>, StoreError> {
        match std::fs::read(self.invoice_pdf_path(&id.to_string())) {
            Ok(bytes) if bytes.len() as u64 <= MAX_PDF_BYTES => Ok(Some(bytes)),
            Ok(_) => Err(StoreError::Io("archived PDF exceeds the size cap".into())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Number + persist an invoice with the lock **already** held.
    pub(super) fn create_invoice_inner(&self, mut invoice: Invoice) -> Result<Invoice, StoreError> {
        // QA-05: the collection cap is checked before a new document is
        // numbered and persisted, under this same lock.
        self.ensure_capacity::<Invoice>()?;
        std::fs::create_dir_all(self.root.join("invoices"))?;
        let prefix = self
            .get_customer(invoice.customer_id)?
            .map(|customer| customer.invoice_prefix)
            .unwrap_or_default();
        let prefix = crate::domain::normalize_invoice_prefix(&prefix)
            .ok_or_else(|| StoreError::Conflict("customer invoice prefix is invalid".into()))?;
        if !prefix.is_empty() {
            let path = self.root.join("invoices").join(".seq.prefixes.json");
            let mut sequences: std::collections::BTreeMap<String, u64> =
                read_json(&path)?.unwrap_or_default();
            let highest = match sequences.get(&prefix) {
                Some(value) => *value,
                None => {
                    let start = format!("{prefix}-");
                    self.list_invoices()?
                        .iter()
                        .filter_map(|existing| {
                            existing.number.strip_prefix(&start)?.parse::<u64>().ok()
                        })
                        .max()
                        .unwrap_or(0)
                }
            };
            let next = highest.checked_add(1).ok_or_else(|| {
                StoreError::Conflict("invoice prefix sequence is exhausted".into())
            })?;
            sequences.insert(prefix.clone(), next);
            write_json(&path, &sequences)?;
            invoice.number = format!("{prefix}-{next:03}");
            write_json(&self.doc_path::<Invoice>(&invoice.id.to_string()), &invoice)?;
            self.index_invoice_locked(&invoice)?;
            return Ok(invoice);
        }
        let seq_path = self.root.join("invoices").join(".seq.json");
        let stored: u64 = read_json(&seq_path)?.unwrap_or(0);
        let highest = if seq_path.exists() {
            stored
        } else {
            self.list_invoices()?
                .iter()
                .filter_map(|existing| existing.number.strip_prefix("INV-")?.parse::<u64>().ok())
                .max()
                .unwrap_or(0)
                .max(stored)
        };
        let n = highest
            .checked_add(1)
            .ok_or_else(|| StoreError::Conflict("invoice sequence is exhausted".into()))?;
        write_json(&seq_path, &n)?;
        invoice.number = format!("INV-{n:04}");
        write_json(&self.doc_path::<Invoice>(&invoice.id.to_string()), &invoice)?;
        self.index_invoice_locked(&invoice)?;
        Ok(invoice)
    }
}

/// Filename sanitiser for derived download names: invoice numbers are
/// minted by `create_invoice` (default or customer-prefixed), but the hint never carries
/// anything that could steer a path (#50).
/// The `pdf` hint persisted with the invoice (#113).
pub(super) fn pdf_hint(number: &str, archived: &[u8], now: DateTime<Utc>) -> PdfHint {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(archived);
    PdfHint {
        filename: format!("{}.pdf", crate::domain::safe_filename(number)),
        bytes: archived.len() as u64,
        sha256: crate::hex_lower(&h.finalize()),
        archived_at: now,
    }
}

#[cfg(test)]
mod invoice_seq_tests {
    use super::*;
    use crate::domain::{Currency, Invoice, InvoiceStatus};

    fn draft(number: &str) -> Invoice {
        Invoice {
            imported: None,
            id: Uuid::new_v4(),
            number: number.into(),
            customer_id: Uuid::new_v4(),
            currency: Currency("EUR".into()),
            period_from: chrono::NaiveDate::from_ymd_opt(2026, 10, 1).unwrap(),
            period_to: chrono::NaiveDate::from_ymd_opt(2026, 10, 7).unwrap(),
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
            subject: String::new(),
            terms: None,
            document: Default::default(),
        }
    }

    #[test]
    fn deleted_invoice_never_frees_its_number() {
        // Review B3: number must be monotonic, not count-derived.
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("data")).unwrap();
        let a = store.create_invoice(draft("_")).unwrap();
        let b = store.create_invoice(draft("_")).unwrap();
        assert_eq!(
            (a.number.as_str(), b.number.as_str()),
            ("INV-0001", "INV-0002")
        );
        store.delete_invoice(a.id).unwrap();
        let c = store.create_invoice(draft("_")).unwrap();
        assert_eq!(c.number, "INV-0003", "numbers must not be recycled");
        // The counter file is not mistaken for an invoice document.
        assert_eq!(store.list_invoices().unwrap().len(), 2);
    }

    fn prefixed_draft(store: &Store, prefix: &str) -> Invoice {
        let invoice = draft("_");
        let customer = serde_json::from_value(serde_json::json!({
            "id": invoice.customer_id, "name": prefix, "currency": "EUR",
            "default_rate_minor": 0, "active": true, "email": "", "invoice_prefix": prefix
        }))
        .unwrap();
        store.put_customer(&customer).unwrap();
        invoice
    }

    #[test]
    fn prefix_sequences_are_independent_durable_and_shared_without_collisions() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("data");
        let store = Store::open(&root).unwrap();
        for prefix in ["CUST1", "CUST2", "MONKEY"] {
            let invoice = prefixed_draft(&store, prefix);
            let first = store.create_invoice(invoice).unwrap();
            assert_eq!(first.number, format!("{prefix}-001"));
            store.delete_invoice(first.id).unwrap();
        }
        let same_prefix_other_customer = prefixed_draft(&store, "CUST1");
        assert_eq!(
            store
                .create_invoice(same_prefix_other_customer)
                .unwrap()
                .number,
            "CUST1-002"
        );
        drop(store);
        let store = Store::open(&root).unwrap();
        let invoice = prefixed_draft(&store, "CUST1");
        assert_eq!(store.create_invoice(invoice).unwrap().number, "CUST1-003");
        assert_eq!(store.create_invoice(draft("_")).unwrap().number, "INV-0001");
        assert_eq!(store.list_invoices().unwrap().len(), 3);
    }

    #[test]
    fn prefixes_bootstrap_from_highest_existing_number_and_never_wrap() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("data")).unwrap();
        let mut restored = prefixed_draft(&store, "MONKEY");
        restored.number = "MONKEY-042".into();
        store.put_invoice(&restored).unwrap();
        let mut global = draft("INV-0099");
        store.put_invoice(&global).unwrap();
        let next = prefixed_draft(&store, "MONKEY");
        assert_eq!(store.create_invoice(next).unwrap().number, "MONKEY-043");
        assert_eq!(store.create_invoice(draft("_")).unwrap().number, "INV-0100");
        write_json(
            &store.root.join("invoices/.seq.prefixes.json"),
            &serde_json::json!({"MONKEY": u64::MAX}),
        )
        .unwrap();
        global.customer_id = restored.customer_id;
        assert!(store.create_invoice(global).is_err());
        assert_eq!(store.list_invoices().unwrap().len(), 4);
    }

    #[test]
    fn concurrent_prefix_allocations_are_unique() {
        let dir = tempfile::tempdir().unwrap();
        let store = std::sync::Arc::new(Store::open(dir.path().join("data")).unwrap());
        let invoice = prefixed_draft(&store, "MONKEY");
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let store = store.clone();
                let mut invoice = invoice.clone();
                invoice.id = Uuid::new_v4();
                std::thread::spawn(move || store.create_invoice(invoice).unwrap().number)
            })
            .collect();
        let numbers: std::collections::BTreeSet<_> = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect();
        assert_eq!(numbers.len(), 8);
        assert!(numbers.contains("MONKEY-001"));
        assert!(numbers.contains("MONKEY-008"));
    }
}
