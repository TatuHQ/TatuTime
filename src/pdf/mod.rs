//! Deterministic invoice PDF rendering (#113).
//!
//! Architecture (ADR on #113, "route B"): `pdf-writer` (the Typst emitter)
//! guarantees valid PDF *syntax*; the invoice *layout* is ours. Content is a
//! block document model — `Doc` — built by the pure `doc_for(&Invoice,
//! &Customer, &Org)` from the persisted snapshot; `render_invoice_pdf(&Doc)`
//! only lays out and emits bytes. #116 (templates) replaces `doc_for`'s
//! hard-coded strings with `%variable%`-interpolated markdown, feeding the
//! **same** `Doc` — the emitter never sees HTML and never needs to change.
//!
//! Determinism contract: `render_invoice_pdf` is pure — no I/O, no clock, no
//! randomness. The same `Doc` in produces byte-identical output, which is
//! what makes the archived `invoices/<id>.pdf` verifiable (`sha256` hint,
//! `ETag`) and lets a legacy invoice re-render to the exact same bytes on
//! first download. There is deliberately no `/Info` dictionary and no file
//! ID, because those would carry timestamps.
//!
//! Watermark (Subtask C): a `Doc` may carry a `watermark` stage, rendered as a
//! light-grey diagonal text stamp (`DRAFT`/`SENT`/`PAID`). The stamp is
//! deliberately **text-only and date-free**: a date would vary between two
//! renders of the same `Doc` and break the byte-stability/`ETag` contract on
//! the on-demand preview path. It is emitted with a rotated text matrix (see
//! `Op::TextRotated`), needs no `ExtGState`/alpha, and is stamped once per page.
//!
//! Money discipline: amounts are formatted from persisted minor units via
//! `api::money_for_email` (integer division), and the totals section prints
//! `invoice.total_minor` directly — layout arithmetic never re-sums printed
//! line amounts, and floats never enter the money path. Layout coordinates
//! use integer point arithmetic; f32 appears only at the pdf-writer boundary.
//!
//! Fonts: base-14 Type1 (`Helvetica` family) with `WinAnsiEncoding` — no
//! font asset, no parser. Scope line: Latin text only; characters outside
//! WinAnsi render as `?` (the agreed trigger to revisit is a non-Latin-1
//! customer name).

mod content;
mod layout;
mod metrics;
mod model;

pub use content::render_invoice_pdf;
pub use model::{Block, Doc, LineRow, Org, Span, doc_for, doc_for_labeled, parse_accent};
