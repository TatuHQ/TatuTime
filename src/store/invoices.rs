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
        let seq_path = self.root.join("invoices").join(".seq.json");
        let stored: u64 = read_json(&seq_path)?.unwrap_or(0);
        // #255: the count-derived guard only exists to bootstrap a pre-ledger
        // data dir (one written before B3 introduced `.seq.json`). Once the
        // ledger exists it is authoritative — the earlier unconditional
        // `list_invoices()` re-listed every invoice under the write lock on
        // every creation, which the sequence file already made unnecessary.
        let n = if seq_path.exists() {
            stored + 1
        } else {
            stored.max(self.list_invoices()?.len() as u64) + 1
        };
        write_json(&seq_path, &n)?;
        invoice.number = format!("INV-{n:04}");
        write_json(&self.doc_path::<Invoice>(&invoice.id.to_string()), &invoice)?;
        self.index_invoice_locked(&invoice)?;
        Ok(invoice)
    }
}

/// Filename sanitiser for derived download names: invoice numbers are
/// minted by `create_invoice` (`INV-%04d`), but the hint never carries
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
}
