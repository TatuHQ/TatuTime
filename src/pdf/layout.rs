use super::metrics::{measure, winansi, wrap_limited};
use super::model::{Block, Doc, LineRow, Span};
use crate::domain::InvoiceWatermarkStage;

// ============================================================== renderer ===

// A4 at 72 dpi, integer points.
pub(super) const PAGE_W: i32 = 595;
pub(super) const PAGE_H: i32 = 842;
const MARGIN: i32 = 50;
const CONTENT_W: i32 = PAGE_W - 2 * MARGIN;
/// Content stops here; below is footer whitespace, above triggers a page break.
const BOTTOM: i32 = MARGIN + 40;

// Table columns (sum = CONTENT_W).
const COL_DATE: i32 = 70;
pub(super) const COL_DESC: i32 = 195;
const COL_HOURS: i32 = 55;
const COL_RATE: i32 = 75;
const COL_AMOUNT: i32 = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Face {
    Regular,
    Bold,
    Oblique,
    BoldOblique,
}

impl Face {
    fn of(bold: bool, italic: bool) -> Self {
        match (bold, italic) {
            (false, false) => Face::Regular,
            (true, false) => Face::Bold,
            (false, true) => Face::Oblique,
            (true, true) => Face::BoldOblique,
        }
    }
    pub(super) fn name(self) -> &'static str {
        match self {
            Face::Regular => "F1",
            Face::Bold => "F2",
            Face::Oblique => "F3",
            Face::BoldOblique => "F4",
        }
    }
    pub(super) fn base(self) -> &'static [u8] {
        match self {
            Face::Regular => b"Helvetica",
            Face::Bold => b"Helvetica-Bold",
            Face::Oblique => b"Helvetica-Oblique",
            Face::BoldOblique => b"Helvetica-BoldOblique",
        }
    }
}

/// A laid-out drawing operation in integer points, top-left-origin page
/// space; flipped to PDF's bottom-up space at emit time.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Op {
    /// Set fill+stroke color (#146 accent); emitted raw for determinism.
    Color { rgb: [f32; 3] },
    Text {
        face: Face,
        size: i32,
        x: i32,
        y: i32,
        bytes: Vec<u8>,
    },
    /// Right-aligned text (numeric table columns).
    TextRight {
        face: Face,
        size: i32,
        x_right: i32,
        y: i32,
        bytes: Vec<u8>,
    },
    /// The watermark stamp (Subtask C): the same glyph run as `Text`, but
    /// emitted with a rotated text matrix so it reads diagonally. `(x, y)` is
    /// the baseline start in top-left-origin page space, chosen by the layout
    /// so the run is centred on the page.
    TextRotated {
        face: Face,
        size: i32,
        x: i32,
        y: i32,
        bytes: Vec<u8>,
    },
    Line {
        x1: i32,
        y1: i32,
        x2: i32,
        y2: i32,
        width: i32,
    },
}

struct Pager {
    pages: Vec<Vec<Op>>,
    cur: Vec<Op>,
    /// Baseline cursor of the next text line, in top-down page points.
    y: i32,
}

impl Pager {
    fn new() -> Self {
        Self {
            pages: Vec::new(),
            cur: Vec::new(),
            y: MARGIN,
        }
    }

    /// Ensure `height` points of room remain; page-break otherwise. Returns
    /// whether a break happened.
    fn need(&mut self, height: i32) -> bool {
        if self.y + height <= PAGE_H - BOTTOM {
            return false;
        }
        let page = std::mem::take(&mut self.cur);
        self.pages.push(page);
        self.y = MARGIN;
        true
    }

    /// Consume `height` vertical space.
    fn advance(&mut self, height: i32) {
        self.y += height;
    }

    fn finish(mut self) -> Vec<Vec<Op>> {
        self.pages.push(std::mem::take(&mut self.cur));
        self.pages
    }
}

// Watermark (Subtask C): a light-grey diagonal stage stamp. A single solid
// grey needs no `ExtGState`/alpha (which this renderer never emits) and keeps
// the op set closed; it stays legible over the invoice without obscuring the
// black content.
/// Watermark ink: a light solid grey (`1.0` = black, `0.0` = white). A
/// DeviceGray fill, so it can never be confused with the RGB accent (#146).
pub(super) const WATERMARK_GREY: f32 = 0.75;
/// Watermark glyph height in points — large enough to read across the page.
const WATERMARK_SIZE: i32 = 60;
/// `cos45 = sin45` in ten-thousandths, for the integer-only diagonal centring:
/// the run advances `0.7071` point right and up per point of its width.
const ROT_DIAGONAL: i32 = 7071;

/// The stamp text for a stage. Deliberately **date-free** (`DRAFT`/`SENT`/
/// `PAID`, never `DRAFT 2026-01-01`): a date would differ between two renders
/// of the same `Doc` and break the byte-stability/`ETag` contract on the
/// on-demand preview path (see the module determinism note).
fn watermark_text(stage: InvoiceWatermarkStage) -> &'static str {
    match stage {
        InvoiceWatermarkStage::Draft => "DRAFT",
        InvoiceWatermarkStage::Sent => "SENT",
        InvoiceWatermarkStage::Paid => "PAID",
    }
}

/// The watermark op for one page, drawn **first** so the invoice ink sits on
/// top of the grey (no alpha: a light grey underneath reads as a background
/// stamp). `put_text_rotated` sets and restores the grey ink itself, so the op
/// set needs no extra colour ops. The run is centred on the page: the anchor
/// is the baseline start, half the run's 45° diagonal extent down-left of the
/// page centre — integer arithmetic only, like the rest of the layout.
fn watermark_ops(stage: InvoiceWatermarkStage) -> Vec<Op> {
    let bytes = winansi(watermark_text(stage));
    let face = Face::Bold;
    let dx = measure(face, WATERMARK_SIZE, &bytes) / 2 * ROT_DIAGONAL / 10_000;
    vec![Op::TextRotated {
        face,
        size: WATERMARK_SIZE,
        x: PAGE_W / 2 - dx,
        y: PAGE_H / 2 + dx,
        bytes,
    }]
}

/// Lay the `Doc` out into per-page operation lists.
pub(super) fn layout(doc: &Doc) -> Vec<Vec<Op>> {
    let mut p = Pager::new();

    // Title.
    let title = winansi(&doc.title);
    p.cur.push(Op::Text {
        face: Face::Bold,
        size: 18,
        x: MARGIN,
        y: p.y + 18,
        bytes: title,
    });
    p.advance(18 + 16);
    p.cur.push(Op::Line {
        x1: MARGIN,
        y1: p.y,
        x2: MARGIN + CONTENT_W,
        y2: p.y,
        width: 1,
    });
    p.advance(10);

    // Header key/values, then free blocks.
    for b in &doc.header {
        emit_block(&mut p, b, 10, 0);
    }
    p.advance(8);
    for b in &doc.blocks {
        emit_block(&mut p, b, 10, 0);
    }
    p.advance(10);

    // Line table with a repeating header.
    if !doc.lines.is_empty() {
        emit_table_head(&mut p, doc);
        for row in &doc.lines {
            emit_table_row(&mut p, row, doc);
        }
        p.advance(6);
    }

    // Totals (bold rule above; prints total_minor, never a re-sum).
    // #146: the configured accent paints the money section only.
    if let Some(rgb) = doc.accent {
        p.cur.push(Op::Color { rgb });
    }
    for b in &doc.totals {
        emit_block(&mut p, b, 11, 0);
    }
    if doc.accent.is_some() {
        p.cur.push(Op::Color {
            rgb: [0.0, 0.0, 0.0],
        });
    }
    p.advance(14);

    // Footer.
    for b in &doc.footer {
        emit_block(&mut p, b, 9, 0);
    }

    let mut pages = p.finish();
    // Watermark (Subtask C): stamp every page with the doc's stage, if any.
    // Drawn first so the invoice ink sits on top of the grey; the stamp is
    // per-page, so a multi-page invoice is stamped throughout.
    if let Some(stage) = doc.watermark {
        let ops = watermark_ops(stage);
        for page in &mut pages {
            let mut stamped = ops.clone();
            stamped.append(page);
            *page = stamped;
        }
    }
    pages
}

fn emit_table_head(p: &mut Pager, doc: &Doc) {
    p.need(30);
    let y = p.y + 12;
    let cols = [
        ("Date", MARGIN, COL_DATE, false),
        (
            if doc.column_labels[0].is_empty() {
                "Description"
            } else {
                &doc.column_labels[0]
            },
            MARGIN + COL_DATE,
            COL_DESC,
            false,
        ),
        (
            if doc.column_labels[1].is_empty() {
                "Hours"
            } else {
                &doc.column_labels[1]
            },
            MARGIN + COL_DATE + COL_DESC,
            COL_HOURS,
            true,
        ),
        (
            if doc.column_labels[2].is_empty() {
                "Rate"
            } else {
                &doc.column_labels[2]
            },
            MARGIN + COL_DATE + COL_DESC + COL_HOURS,
            COL_RATE,
            true,
        ),
        (
            "Amount",
            MARGIN + COL_DATE + COL_DESC + COL_HOURS + COL_RATE,
            COL_AMOUNT,
            true,
        ),
    ];
    for (label, x, w, right) in cols {
        let bytes = winansi(label);
        p.cur.push(if right {
            Op::TextRight {
                face: Face::Bold,
                size: 9,
                x_right: x + w,
                y,
                bytes,
            }
        } else {
            Op::Text {
                face: Face::Bold,
                size: 9,
                x,
                y,
                bytes,
            }
        });
    }
    p.y = y + 4;
    p.cur.push(Op::Line {
        x1: MARGIN,
        y1: p.y,
        x2: MARGIN + CONTENT_W,
        y2: p.y,
        width: 1,
    });
    p.advance(6);
}

fn emit_table_row(p: &mut Pager, row: &LineRow, doc: &Doc) {
    // Description wraps to at most two lines in its column.
    let desc_bytes = winansi(&row.description);
    let desc_lines = wrap_limited(Face::Regular, 9, &desc_bytes, COL_DESC - 6, 2);
    let height = 12 * desc_lines.len() as i32 + 4;
    if p.need(height) {
        emit_table_head(p, doc);
    }
    let base = p.y + 11;
    p.cur.push(Op::Text {
        face: Face::Regular,
        size: 9,
        x: MARGIN,
        y: base,
        bytes: winansi(&row.date),
    });
    for (i, l) in desc_lines.into_iter().enumerate() {
        p.cur.push(Op::Text {
            face: Face::Regular,
            size: 9,
            x: MARGIN + COL_DATE,
            y: base + 11 * i as i32,
            bytes: l,
        });
    }
    let mut num = |s: &str, x_right: i32| {
        if s.is_empty() {
            return;
        }
        p.cur.push(Op::TextRight {
            face: Face::Regular,
            size: 9,
            x_right,
            y: base,
            bytes: winansi(s),
        });
    };
    num(&row.hours, MARGIN + COL_DATE + COL_DESC + COL_HOURS - 4);
    num(
        &row.rate,
        MARGIN + COL_DATE + COL_DESC + COL_HOURS + COL_RATE - 4,
    );
    num(
        &row.amount,
        MARGIN + COL_DATE + COL_DESC + COL_HOURS + COL_RATE + COL_AMOUNT - 4,
    );
    p.advance(height);
}

/// Emit one flowing block at `size` points. `indent` nests bullets/lists.
fn emit_block(p: &mut Pager, block: &Block, size: i32, indent: i32) {
    match block {
        Block::Heading { level, spans } => {
            let h = match level {
                1 => 14,
                2 => 12,
                _ => 11,
            };
            emit_spans(p, spans, h, true, indent);
        }
        Block::Paragraph(spans) => emit_spans(p, spans, size, false, indent),
        Block::Bullet(spans) => emit_spans_with_prefix(p, spans, size, indent, b"\x95 "),
        Block::Numbered { number, spans } => {
            emit_spans_with_prefix(p, spans, size, indent, &winansi(&format!("{number}. ")))
        }
        Block::KeyVal { key, spans } => emit_keyval(p, key, spans, size, indent),
    }
}

/// A `Key: value` line: bold key in a fixed label column, value wrapped.
fn emit_keyval(p: &mut Pager, key: &str, spans: &[Span], size: i32, indent: i32) {
    const LABEL_W: i32 = 110;
    let key_bytes = winansi(&format!("{key}:"));
    let x0 = MARGIN + indent;
    let avail = CONTENT_W - indent - LABEL_W;
    let mut lines = wrap_spans(spans, size, avail, false);
    if lines.is_empty() {
        lines.push(vec![]);
    }
    let height = 13 * lines.len() as i32;
    p.need(height);
    let base = p.y + size;
    p.cur.push(Op::Text {
        face: Face::Bold,
        size,
        x: x0,
        y: base,
        bytes: key_bytes,
    });
    let xv = x0 + LABEL_W;
    for (i, line) in lines.into_iter().enumerate() {
        emit_line_ops(p, &line, size, xv, base + 13 * i as i32);
    }
    p.advance(height);
}

/// Styled paragraph: wrap across spans (each span keeps its own face), then
/// emit line by line with running x.
fn emit_spans(p: &mut Pager, spans: &[Span], size: i32, force_bold: bool, indent: i32) {
    let lines = wrap_spans(spans, size, CONTENT_W - indent, force_bold);
    let height = 13 * lines.len() as i32;
    p.need(height);
    let base = p.y + size;
    for (i, line) in lines.into_iter().enumerate() {
        emit_line_ops(p, &line, size, MARGIN + indent, base + 13 * i as i32);
    }
    p.advance(height);
}

fn emit_spans_with_prefix(p: &mut Pager, spans: &[Span], size: i32, indent: i32, prefix: &[u8]) {
    let px = measure(Face::Regular, size, prefix);
    let lines = wrap_spans(spans, size, CONTENT_W - indent - px, false);
    let height = 13 * lines.len() as i32;
    p.need(height);
    let base = p.y + size;
    p.cur.push(Op::Text {
        face: Face::Regular,
        size,
        x: MARGIN + indent,
        y: base,
        bytes: prefix.to_vec(),
    });
    for (i, line) in lines.into_iter().enumerate() {
        emit_line_ops(p, &line, size, MARGIN + indent + px, base + 13 * i as i32);
    }
    p.advance(height);
}

/// One laid-out word of a line: the span it came from (face) + its bytes.
type Word = (bool, bool, Vec<u8>);

/// Greedy multi-span wrap: words carry their span styling; measurement uses
/// the exact face per word.
fn wrap_spans(spans: &[Span], size: i32, max_w: i32, force_bold: bool) -> Vec<Vec<Word>> {
    let space = measure(Face::Regular, size, b" ");
    let mut lines: Vec<Vec<Word>> = Vec::new();
    let mut cur: Vec<Word> = Vec::new();
    let mut cur_w = 0;
    for s in spans {
        if s.text.is_empty() {
            continue;
        }
        let bold = s.bold || force_bold;
        let bytes = winansi(&s.text);
        for word in bytes.split(|b| *b == b' ' || *b == b'\n') {
            if word.is_empty() {
                continue;
            }
            let face = Face::of(bold, s.italic);
            let mut w = measure(face, size, word);
            let mut word = word;
            while w > max_w && word.len() > 1 {
                let mut n = word.len();
                while n > 1 && measure(face, size, &word[..n]) > max_w {
                    n -= 1;
                }
                let (head, tail) = word.split_at(n);
                if !cur.is_empty() {
                    lines.push(std::mem::take(&mut cur));
                    cur_w = 0;
                }
                lines.push(vec![(bold, s.italic, head.to_vec())]);
                word = tail;
                w = measure(face, size, word);
            }
            let add = if cur.is_empty() { w } else { w + space };
            if cur_w + add > max_w && !cur.is_empty() {
                lines.push(std::mem::take(&mut cur));
                cur_w = 0;
            }
            if !cur.is_empty() {
                cur_w += space;
            }
            cur.push((bold, s.italic, word.to_vec()));
            cur_w += w;
        }
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    if lines.is_empty() {
        lines.push(vec![]);
    }
    lines
}

/// Emit one wrapped line of styled words at `(x, y)` with a running cursor.
fn emit_line_ops(p: &mut Pager, line: &[Word], size: i32, x0: i32, y: i32) {
    let space = measure(Face::Regular, size, b" ");
    let mut x = x0;
    for (i, (bold, italic, bytes)) in line.iter().enumerate() {
        if i > 0 {
            x += space;
        }
        let face = Face::of(*bold, *italic);
        let w = measure(face, size, bytes);
        p.cur.push(Op::Text {
            face,
            size,
            x,
            y,
            bytes: bytes.clone(),
        });
        x += w;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{
        Currency, Customer, Invoice, InvoiceLine, InvoiceStatus, InvoiceWatermarkStage, LineKind,
    };
    use crate::pdf::{Org, doc_for, render_invoice_pdf};
    use chrono::{TimeZone, Utc};

    fn sample_invoice() -> Invoice {
        Invoice {
            id: "11111111-1111-1111-1111-111111111111".parse().unwrap(),
            number: "INV-0001".into(),
            customer_id: "22222222-2222-2222-2222-222222222222".parse().unwrap(),
            currency: Currency("EUR".into()),
            period_from: chrono::NaiveDate::from_ymd_opt(2026, 10, 1).unwrap(),
            period_to: chrono::NaiveDate::from_ymd_opt(2026, 10, 31).unwrap(),
            lines: vec![
                InvoiceLine {
                    kind: LineKind::Time,
                    date: chrono::NaiveDate::from_ymd_opt(2026, 10, 5).unwrap(),
                    entry_id: None,
                    expense_id: None,
                    project_code: Some(crate::domain::ProjectCode("WEB-1".into())),
                    task_code: None,
                    hours: Some(crate::domain::Hours(800)),
                    rate_minor: Some(9000),
                    amount_minor: 72000,
                    note: String::new(),
                    quantity_hundredths: None,
                    unit_price_minor: None,
                    item_kind: None,
                },
                InvoiceLine {
                    kind: LineKind::Expense,
                    date: chrono::NaiveDate::from_ymd_opt(2026, 10, 9).unwrap(),
                    entry_id: None,
                    expense_id: None,
                    project_code: None,
                    task_code: None,
                    hours: None,
                    rate_minor: None,
                    amount_minor: 12500,
                    note: "Flight tickets".into(),
                    quantity_hundredths: None,
                    unit_price_minor: None,
                    item_kind: None,
                },
            ],
            total_minor: 84500,
            status: InvoiceStatus::Issued,
            created_at: Utc.timestamp_opt(1_700_000_000, 0).unwrap(),
            issued_at: Some(Utc.timestamp_opt(1_760_000_000, 0).unwrap()),
            due_date: Some(chrono::NaiveDate::from_ymd_opt(2026, 11, 14).unwrap()),
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

    fn customer() -> Customer {
        Customer {
            id: "22222222-2222-2222-2222-222222222222".parse().unwrap(),
            name: "Capybara Solutions".into(),
            currency: Currency("EUR".into()),
            default_rate_minor: 9000,
            active: true,
            email: "billing@capybara.example".into(),
            payment_terms: None,
            invoice_notes: String::new(),
            invoice_subject: String::new(),
            address: None,
            contacts: vec![],
            tax_hundredths: 0,
            discount_hundredths: 0,
            payment_details: None,
        }
    }

    fn org() -> Org {
        Org::named("Tucano")
    }

    #[test]
    fn many_lines_flow_onto_more_pages() {
        let mut inv = sample_invoice();
        inv.lines = (0..80)
            .map(|i| InvoiceLine {
                kind: LineKind::Fixed,
                date: inv.period_from,
                entry_id: None,
                expense_id: None,
                project_code: None,
                task_code: None,
                hours: None,
                rate_minor: None,
                amount_minor: i as u64 * 100,
                note: format!("Recurring charge {i}"),
                quantity_hundredths: None,
                unit_price_minor: None,
                item_kind: None,
            })
            .collect();
        let doc = doc_for(&inv, &customer(), &org());
        let pages = layout(&doc);
        assert!(pages.len() > 1, "80 lines must flow across pages");
        let bytes = render_invoice_pdf(&doc);
        let parsed = lopdf::Document::load_mem(&bytes).expect("re-parse multi-page");
        assert_eq!(parsed.get_pages().len(), pages.len());
    }

    #[test]
    fn spans_carry_bold_and_italic_faces() {
        let doc = Doc {
            title: "T".into(),
            column_labels: Default::default(),
            accent: None,
            watermark: None,
            header: vec![],
            blocks: vec![Block::Paragraph(vec![
                Span::bold("B"),
                Span::text(" "),
                Span::italic("I"),
                Span::text(" plain"),
            ])],
            lines: vec![],
            totals: vec![],
            footer: vec![],
        };
        let pages = layout(&doc);
        let faces: Vec<Face> = pages[0]
            .iter()
            .filter_map(|op| match op {
                Op::Text { face, .. }
                | Op::TextRight { face, .. }
                | Op::TextRotated { face, .. } => Some(*face),
                Op::Line { .. } | Op::Color { .. } => None,
            })
            .collect();
        assert!(faces.contains(&Face::Bold));
        assert!(faces.contains(&Face::Oblique));
        assert!(faces.contains(&Face::Regular));
    }

    /// Whether the (uncompressed) content stream carries the `(STAMP)` literal.
    fn has_stamp(bytes: &[u8], stamp: &str) -> bool {
        let needle = format!("({stamp})");
        bytes.windows(needle.len()).any(|w| w == needle.as_bytes())
    }

    #[test]
    fn watermark_stamp_is_present_per_stage_and_deterministic() {
        // The determinism contract in use: the same staged `Doc` renders
        // byte-identically, and the stamp glyphs are in the re-parsed content.
        for (stage, text) in [
            (InvoiceWatermarkStage::Draft, "DRAFT"),
            (InvoiceWatermarkStage::Sent, "SENT"),
            (InvoiceWatermarkStage::Paid, "PAID"),
        ] {
            let mut doc = doc_for(&sample_invoice(), &customer(), &org());
            doc.watermark = Some(stage);
            let a = render_invoice_pdf(&doc);
            let b = render_invoice_pdf(&doc);
            assert_eq!(a, b, "{text}: same Doc renders byte-identically");
            let _ = lopdf::Document::load_mem(&a).expect("re-parse stamped pdf");
            assert!(has_stamp(&a, text), "the {text} stamp is in the content");
        }
    }

    #[test]
    fn no_stage_leaves_the_document_unstamped() {
        let bytes = render_invoice_pdf(&doc_for(&sample_invoice(), &customer(), &org()));
        for text in ["DRAFT", "SENT", "PAID"] {
            assert!(!has_stamp(&bytes, text), "no {text} stamp without a stage");
        }
    }

    #[test]
    fn watermark_is_a_rotated_grey_run_on_every_page() {
        let mut inv = sample_invoice();
        inv.lines = (0..80)
            .map(|i| InvoiceLine {
                kind: LineKind::Fixed,
                date: inv.period_from,
                entry_id: None,
                expense_id: None,
                project_code: None,
                task_code: None,
                hours: None,
                rate_minor: None,
                amount_minor: i as u64 * 100,
                note: format!("Recurring charge {i}"),
                quantity_hundredths: None,
                unit_price_minor: None,
                item_kind: None,
            })
            .collect();
        let mut doc = doc_for(&inv, &customer(), &org());
        doc.watermark = Some(InvoiceWatermarkStage::Draft);
        let pages = layout(&doc);
        assert!(pages.len() > 1, "the fixture spans pages");
        for page in &pages {
            // The stamp is the first op on the page, so the invoice ink that
            // follows is drawn on top of the light grey run.
            assert!(
                matches!(page.first(), Some(Op::TextRotated { .. })),
                "the stamp is drawn first"
            );
        }
    }
}
