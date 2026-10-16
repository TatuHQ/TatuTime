//! Deterministic invoice PDF rendering (#113).
//!
//! `pdf-writer` guarantees valid PDF *syntax*; the invoice *layout* is ours.
//! Content is the `Doc` block model, built from the persisted snapshot by
//! `doc_for` and laid out by `render_invoice_pdf` — the emitter never sees HTML
//! and never needs to change. `render_invoice_pdf` is pure (no I/O, clock or
//! randomness), so the same `Doc` yields byte-identical output, which is what
//! makes the archived `invoices/<id>.pdf` verifiable (`sha256`, `ETag`) and lets a
//! legacy invoice re-render to the same bytes. A `Doc` may carry a text-only,
//! date-free watermark (`DRAFT`/`SENT`/`PAID`); money is formatted from persisted
//! minor units, fonts are base-14 Type1 (`WinAnsiEncoding`, Latin text only).

mod content;
mod layout;
mod metrics;
mod model;

pub use content::render_invoice_pdf;
pub use model::{Block, Doc, LineRow, Org, Span, doc_for, doc_for_labeled, parse_accent};
