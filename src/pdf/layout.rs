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
/// Baseline of the `Page N of M` footer, inside the bottom margin.
const FOOTER_Y: i32 = PAGE_H - MARGIN + 18;

// Table columns (sum = CONTENT_W): the `Item type` column replaces the old
// per-line `Date` column, which the metadata block now owns.
const COL_ITEM: i32 = 62;
pub(super) const COL_DESC: i32 = 228;
const COL_QTY: i32 = 55;
const COL_UNIT: i32 = 70;
const COL_AMOUNT: i32 = 80;

// Two identity columns: issuer on the left, recipient on the right. Both start
// at the same y so `From` and `Invoice for` read as a pair.
const COLUMN_GAP: i32 = 24;
const COLUMN_W: i32 = (CONTENT_W - COLUMN_GAP) / 2;
const META_LABEL_W: i32 = 72;
const META_SIZE: i32 = 9;
const META_LINE: i32 = 13;

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
        face: Face::Regular,
        size: 28,
        x: MARGIN,
        y: p.y + 28,
        bytes: title,
    });
    // The subject is the document's subtitle: it sits directly under the title,
    // separated by one blank line. Its label keeps the template override while
    // the value starts right after the measured label (not in the wide
    // key/value column used by the metadata rows).
    let subject_block =
        (doc.subject.is_some() && !doc.meta.is_empty()).then(|| doc.meta[0].clone());
    let subject_rows = usize::from(subject_block.is_some());
    p.y = MARGIN + 28;
    if let Some(block) = &subject_block {
        p.advance(13); // one blank line below the title
        emit_subject(&mut p, block, 10);
    }
    p.advance(18);

    // Invoice identity (Invoice ID, PO, issue/due dates, period) above the
    // two identity columns, in a compact two-column grid. This is the first
    // block after the title/subject and replaces the old right-hand column.
    let meta_top = p.y;
    p.y = emit_meta_grid(&mut p, &doc.meta[subject_rows..], meta_top);
    p.advance(18);

    // Issuer and recipient columns start at the same y and use the same width,
    // so `From` and `Invoice for` are aligned. Their heights are independent;
    // the flow continues below the taller one.
    let recipient = doc
        .header
        .iter()
        .enumerate()
        .filter(|(_, block)| matches!(block, Block::Heading { .. }))
        .nth(1)
        .map(|(index, _)| index);
    let (issuer, customer) = match recipient {
        Some(index) => (&doc.header[..index], &doc.header[index..]),
        None => (&doc.header[..], &[][..]),
    };
    let columns_top = p.y;
    let (issuer_ops, issuer_bottom) = render_column(issuer, MARGIN, COLUMN_W, columns_top);
    let (customer_ops, customer_bottom) = render_column(
        customer,
        MARGIN + COLUMN_W + COLUMN_GAP,
        COLUMN_W,
        columns_top,
    );
    p.cur.extend(issuer_ops);
    p.cur.extend(customer_ops);
    p.y = issuer_bottom.max(customer_bottom);
    p.advance(18);

    // Free blocks (template body).
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

    // Totals (prints persisted minor units, never a re-sum). The values are
    // right-aligned under the Amount column and labels under Unit price, so the
    // money block reads as the table's final rows rather than a floating card.
    // #146: the configured accent paints the money section only.
    if let Some(rgb) = doc.accent {
        p.cur.push(Op::Color { rgb });
    }
    p.need((doc.totals.len() as i32 * 13) + 26);
    emit_totals(&mut p, doc);
    if doc.accent.is_some() {
        p.cur.push(Op::Color {
            rgb: [0.0, 0.0, 0.0],
        });
    }
    p.advance(14);

    // Footer.
    for b in &doc.footer {
        let heading = matches!(b, Block::Heading { .. });
        if heading {
            p.need(52);
        }
        emit_block(&mut p, b, 9, 0);
        // A little breathing room between a section heading (Payment details,
        // Notes) and its first row; the rows themselves keep the tight rhythm.
        if heading {
            p.advance(4);
        }
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
    // Page footer, in a post-pass: the generator credit is pinned to the page
    // footer on every page (left), while `Page N of M` needs the final page
    // count and sits right-aligned on the same baseline.
    let total = pages.len();
    for (i, page) in pages.iter_mut().enumerate() {
        if !doc.credit.trim().is_empty() {
            page.push(Op::Text {
                face: Face::Oblique,
                size: 8,
                x: MARGIN,
                y: FOOTER_Y,
                bytes: winansi(&doc.credit),
            });
        }
        let bytes = winansi(&format!("Page {} of {}", i + 1, total));
        let width = measure(Face::Regular, 8, &bytes);
        page.push(Op::Text {
            face: Face::Regular,
            size: 8,
            x: PAGE_W - MARGIN - width,
            y: FOOTER_Y,
            bytes,
        });
    }
    pages
}

/// Emit the invoice identity as a compact two-column grid above the identity
/// columns. Every metadata row is a `KeyVal`; anything else is ignored (the
/// grid is layout-owned, not template content). Returns the bottom of the grid.
fn emit_meta_grid(p: &mut Pager, blocks: &[Block], top: i32) -> i32 {
    let items: Vec<(&str, &[Span])> = blocks
        .iter()
        .filter_map(|block| match block {
            Block::KeyVal { key, spans } => Some((key.as_str(), spans.as_slice())),
            _ => None,
        })
        .collect();
    let mut y = top;
    for pair in items.chunks(2) {
        let mut row_height = META_LINE;
        for (column, (key, spans)) in pair.iter().enumerate() {
            let x0 = MARGIN + column as i32 * (COLUMN_W + COLUMN_GAP);
            let key_bytes = winansi(&format!("{key}:"));
            let lines = wrap_spans(spans, META_SIZE, COLUMN_W - META_LABEL_W, false);
            let height = META_LINE * lines.len() as i32;
            let base = y + META_SIZE;
            p.cur.push(Op::Text {
                face: Face::Bold,
                size: META_SIZE,
                x: x0,
                y: base,
                bytes: key_bytes,
            });
            let xv = x0 + META_LABEL_W;
            for (i, line) in lines.into_iter().enumerate() {
                emit_line_ops(p, &line, META_SIZE, xv, base + META_LINE * i as i32);
            }
            row_height = row_height.max(height);
        }
        y += row_height;
    }
    y
}

/// Lay out one identity column into its own operation list, without advancing
/// the shared page cursor. `x` is the absolute left edge and `width` the wrap
/// width; both `From` blocks and `Invoice for` blocks use this so they align.
fn render_column(blocks: &[Block], x: i32, width: i32, top: i32) -> (Vec<Op>, i32) {
    let mut column = Pager::new();
    column.y = top;
    let indent = x - MARGIN;
    for block in blocks {
        emit_block_compact(&mut column, block, 10, indent, width + indent);
    }
    (column.cur, column.y)
}

/// Column blocks use the compact label/value gap for the legal-id and company
/// rows; the 110pt metadata label column was designed for a narrower right-hand
/// rail and left `Tax ID:       value` visually disconnected.
fn emit_block_compact(p: &mut Pager, block: &Block, size: i32, indent: i32, max_w: i32) {
    match block {
        Block::KeyVal { key, spans } => emit_keyval_compact(p, key, spans, size, indent, max_w),
        _ => emit_block_in(p, block, size, indent, max_w),
    }
}

/// A compact `Key: value` row: the value starts just after the measured label.
fn emit_keyval_compact(
    p: &mut Pager,
    key: &str,
    spans: &[Span],
    size: i32,
    indent: i32,
    max_w: i32,
) {
    let key_bytes = winansi(&format!("{key}:"));
    let label_w = measure(Face::Bold, size, &key_bytes) + 6;
    let avail = max_w - indent - label_w;
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
        x: MARGIN + indent,
        y: base,
        bytes: key_bytes,
    });
    let xv = MARGIN + indent + label_w;
    for (i, line) in lines.into_iter().enumerate() {
        emit_line_ops(p, &line, size, xv, base + 13 * i as i32);
    }
    p.advance(height);
}

fn emit_table_head(p: &mut Pager, doc: &Doc) {
    p.need(30);
    let y = p.y + 12;
    let columns = [
        (
            if doc.column_labels[0].is_empty() {
                "Item type"
            } else {
                &doc.column_labels[0]
            },
            MARGIN,
            COL_ITEM,
            false,
        ),
        (
            if doc.column_labels[1].is_empty() {
                "Description"
            } else {
                &doc.column_labels[1]
            },
            MARGIN + COL_ITEM,
            COL_DESC,
            false,
        ),
        (
            if doc.column_labels[2].is_empty() {
                "Quantity"
            } else {
                &doc.column_labels[2]
            },
            MARGIN + COL_ITEM + COL_DESC,
            COL_QTY,
            true,
        ),
        (
            if doc.column_labels[3].is_empty() {
                "Unit price"
            } else {
                &doc.column_labels[3]
            },
            MARGIN + COL_ITEM + COL_DESC + COL_QTY,
            COL_UNIT,
            true,
        ),
        (
            "Amount",
            MARGIN + COL_ITEM + COL_DESC + COL_QTY + COL_UNIT,
            COL_AMOUNT,
            true,
        ),
    ];
    for (label, x, w, right) in columns {
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
        bytes: winansi(&row.item_type),
    });
    for (i, line) in desc_lines.into_iter().enumerate() {
        p.cur.push(Op::Text {
            face: Face::Regular,
            size: 9,
            x: MARGIN + COL_ITEM,
            y: base + 11 * i as i32,
            bytes: line,
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
    num(&row.quantity, MARGIN + COL_ITEM + COL_DESC + COL_QTY - 4);
    num(
        &row.unit_price,
        MARGIN + COL_ITEM + COL_DESC + COL_QTY + COL_UNIT - 4,
    );
    num(
        &row.amount,
        MARGIN + COL_ITEM + COL_DESC + COL_QTY + COL_UNIT + COL_AMOUNT - 4,
    );
    p.advance(height);
}

/// Emit one flowing block at `size` points. `indent` nests bullets/lists.
fn emit_block(p: &mut Pager, block: &Block, size: i32, indent: i32) {
    emit_block_in(p, block, size, indent, CONTENT_W);
}

/// Emit one flowing block, wrapping at `max_w` points from the left margin.
/// The default is the full content width; the header passes the left column
/// width so its blocks cannot cross into the metadata column.
fn emit_block_in(p: &mut Pager, block: &Block, size: i32, indent: i32, max_w: i32) {
    match block {
        Block::Heading { level, spans } => {
            let h = match level {
                1 => 14,
                2 => 12,
                _ => 11,
            };
            emit_spans(p, spans, h, true, indent, max_w);
        }
        Block::Paragraph(spans) => emit_spans(p, spans, size, false, indent, max_w),
        Block::Bullet(spans) => emit_spans_with_prefix(p, spans, size, indent, b"\x95 ", max_w),
        Block::Numbered { number, spans } => emit_spans_with_prefix(
            p,
            spans,
            size,
            indent,
            &winansi(&format!("{number}. ")),
            max_w,
        ),
        Block::KeyVal { key, spans } => emit_keyval(p, key, spans, size, indent, max_w),
        Block::Rule { dashed } => {
            p.advance(6);
            p.need(1);
            if *dashed {
                push_dashed_rule(p, max_w);
            } else {
                p.cur.push(Op::Line {
                    x1: MARGIN,
                    y1: p.y,
                    x2: MARGIN + max_w,
                    y2: p.y,
                    width: 1,
                });
            }
            p.advance(6);
        }
    }
}

/// A dashed separator built from 3pt segments every 6pt: `Op::Line` needs no
/// dash-pattern state, so the op set stays closed and the bytes deterministic.
fn push_dashed_rule(p: &mut Pager, max_w: i32) {
    let end = MARGIN + max_w;
    let mut x = MARGIN;
    while x < end {
        let x2 = (x + 3).min(end);
        p.cur.push(Op::Line {
            x1: x,
            y1: p.y,
            x2,
            y2: p.y,
            width: 1,
        });
        x += 6;
    }
}

/// The subject subtitle directly under the document title. Unlike the metadata
/// `Key: value` rows, the value starts immediately after the measured label, so
/// `Subject: October services` reads as one phrase instead of two distant
/// columns. Wraps inside the left column so it never runs under the issuer
/// block.
fn emit_subject(p: &mut Pager, block: &Block, size: i32) {
    let Block::KeyVal { key, spans } = block else {
        return;
    };
    let key_bytes = winansi(&format!("{key}:"));
    let label_w = measure(Face::Bold, size, &key_bytes) + 6;
    // The subtitle sits above the identity columns, so it can use the whole
    // content width instead of wrapping inside the left column.
    let avail = CONTENT_W - label_w;
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
        x: MARGIN,
        y: base,
        bytes: key_bytes,
    });
    let xv = MARGIN + label_w;
    for (i, line) in lines.into_iter().enumerate() {
        emit_line_ops(p, &line, size, xv, base + 13 * i as i32);
    }
    p.advance(height);
}

/// Draw the totals as the table's final rows: labels right-aligned under the
/// Unit-price column, values right-aligned under Amount. Total hours sits on
/// the same first line, with its value under Quantity. One 13pt rhythm for
/// every row keeps the line table and its summary visually identical.
fn emit_totals(p: &mut Pager, doc: &Doc) {
    let value_right = MARGIN + COL_ITEM + COL_DESC + COL_QTY + COL_UNIT + COL_AMOUNT - 4;
    let label_right = MARGIN + COL_ITEM + COL_DESC + COL_QTY + COL_UNIT - 8;
    let hours_right = MARGIN + COL_ITEM + COL_DESC + COL_QTY - 4;
    let mut y = p.y;
    if let Some(hours) = &doc.total_hours {
        let base = y + 9;
        p.cur.push(Op::Text {
            face: Face::Bold,
            size: 9,
            x: MARGIN,
            y: base,
            bytes: winansi("Total hours:"),
        });
        push_text_right(p, Face::Bold, 9, hours_right, base, hours);
    }
    for block in &doc.totals {
        let Block::KeyVal { key, spans } = block else {
            continue;
        };
        let base = y + 9;
        push_text_right(p, Face::Bold, 9, label_right, base, &format!("{key}:"));
        let text: String = spans.iter().map(|span| span.text.as_str()).collect();
        let face = if spans.iter().any(|span| span.bold) {
            Face::Bold
        } else {
            Face::Regular
        };
        push_text_right(p, face, 9, value_right, base, &text);
        y += 13;
    }
    p.y = y;
}

/// Right-align one short run at `x_right`.
fn push_text_right(p: &mut Pager, face: Face, size: i32, x_right: i32, y: i32, text: &str) {
    p.cur.push(Op::TextRight {
        face,
        size,
        x_right,
        y,
        bytes: winansi(text),
    });
}

/// A `Key: value` line: bold key in a fixed label column, value wrapped.
fn emit_keyval(p: &mut Pager, key: &str, spans: &[Span], size: i32, indent: i32, max_w: i32) {
    const LABEL_W: i32 = 110;
    let key_bytes = winansi(&format!("{key}:"));
    let x0 = MARGIN + indent;
    let avail = max_w - indent - LABEL_W;
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
fn emit_spans(p: &mut Pager, spans: &[Span], size: i32, force_bold: bool, indent: i32, max_w: i32) {
    let lines = wrap_spans(spans, size, max_w - indent, force_bold);
    let height = 13 * lines.len() as i32;
    p.need(height);
    let base = p.y + size;
    for (i, line) in lines.into_iter().enumerate() {
        emit_line_ops(p, &line, size, MARGIN + indent, base + 13 * i as i32);
    }
    p.advance(height);
}

fn emit_spans_with_prefix(
    p: &mut Pager,
    spans: &[Span],
    size: i32,
    indent: i32,
    prefix: &[u8],
    max_w: i32,
) {
    let px = measure(Face::Regular, size, prefix);
    let lines = wrap_spans(spans, size, max_w - indent - px, false);
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
        Address, Currency, Customer, Invoice, InvoiceLine, InvoiceStatus, InvoiceWatermarkStage,
        LineKind,
    };
    use crate::pdf::{Org, doc_for, render_invoice_pdf};
    use chrono::{TimeZone, Utc};

    fn sample_invoice() -> Invoice {
        Invoice {
            supersedes: None,
            superseded_by: None,
            imported: None,
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
            subject: String::new(),
            terms: None,
            document: Default::default(),
        }
    }

    fn customer() -> Customer {
        Customer {
            id: "22222222-2222-2222-2222-222222222222".parse().unwrap(),
            name: "Tucano Time".into(),
            currency: Currency("EUR".into()),
            default_rate_minor: 9000,
            active: true,
            email: "billing@tucanotime.example".into(),
            payment_terms: None,
            invoice_notes: String::new(),
            invoice_subject: String::new(),
            invoice_prefix: Default::default(),
            address: None,
            contacts: vec![],
            tax_hundredths: 0,
            discount_hundredths: 0,
            payment_details: None,
            legal_id: String::new(),
        }
    }

    fn org() -> Org {
        Org::named("Tucano")
    }

    /// A burst of `count` fixed lines, to force the document onto several pages.
    fn many_lines(inv: &mut Invoice, count: usize) {
        inv.lines = (0..count)
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
    }

    /// The `Page N of M` text op of a page, if any.
    fn page_footer(page: &[Op]) -> Option<String> {
        page.iter().rev().find_map(|op| match op {
            Op::Text { bytes, .. } if bytes.starts_with(b"Page ") => {
                Some(String::from_utf8_lossy(bytes).into_owned())
            }
            _ => None,
        })
    }

    /// The baseline of the text op whose glyph run is exactly `word`.
    fn y_of(page: &[Op], word: &str) -> Option<i32> {
        page.iter().find_map(|op| match op {
            Op::Text { bytes, y, .. } if bytes == word.as_bytes() => Some(*y),
            _ => None,
        })
    }

    #[test]
    fn column_widths_sum_to_the_content_width() {
        assert_eq!(
            COL_ITEM + COL_DESC + COL_QTY + COL_UNIT + COL_AMOUNT,
            CONTENT_W
        );
    }

    #[test]
    fn many_lines_flow_onto_more_pages() {
        let mut inv = sample_invoice();
        many_lines(&mut inv, 80);
        let doc = doc_for(&inv, &customer(), &org());
        let pages = layout(&doc);
        assert!(pages.len() > 1, "80 lines must flow across pages");
        // Every page carries its `Page N of M` footer.
        for (i, page) in pages.iter().enumerate() {
            assert_eq!(
                page_footer(page).as_deref(),
                Some(format!("Page {} of {}", i + 1, pages.len()).as_str()),
                "page {i} is numbered"
            );
        }
        let bytes = render_invoice_pdf(&doc);
        let parsed = lopdf::Document::load_mem(&bytes).expect("re-parse multi-page");
        assert_eq!(parsed.get_pages().len(), pages.len());
    }

    #[test]
    fn multi_line_addresses_do_not_collapse_into_one_line() {
        let mut c = customer();
        c.address = Some(Address {
            street: "9 Placeholder Road".into(),
            city: "Testville".into(),
            postal_code: "AA1 1AA".into(),
            country: "Freedonia".into(),
        });
        let doc = doc_for(&sample_invoice(), &c, &org());
        let pages = layout(&doc);
        let page = &pages[0];
        // Each address line is its own paragraph, so the words of different
        // lines sit on different baselines.
        let street = y_of(page, "Placeholder").expect("street line");
        let city = y_of(page, "Testville,").expect("city line");
        let postal = y_of(page, "AA1").expect("postal line");
        let country = y_of(page, "Freedonia").expect("country line");
        assert!(street < city && city < postal && postal < country);
        assert_eq!(city - street, 13, "one line each, no collapsing");
    }

    #[test]
    fn header_blocks_wrap_inside_the_left_column() {
        let mut inv = sample_invoice();
        inv.subject = "October services across several projects".into();
        let mut c = customer();
        c.address = Some(Address {
            street: "One Extremely Long Customer Street Name That Should Wrap".into(),
            city: "Testville".into(),
            postal_code: "AA1 1AA".into(),
            country: "Freedonia".into(),
        });
        let mut o = Org::named(
            "Tucano Impossibly Long Issuer Company Name That Must Wrap Inside The Left Column",
        );
        o.legal_id = "VAT BE 0123 4567 8901 2345 6789".into();
        o.address = Some(Address {
            street: "One Exceedingly Long Issuer Street Line That Must Wrap As Well".into(),
            city: "Brussels".into(),
            postal_code: "1000".into(),
            country: "Belgium".into(),
        });
        let doc = doc_for(&inv, &c, &o);
        assert!(
            doc.subject.is_some(),
            "the metadata column carries a subject"
        );
        let pages = layout(&doc);
        assert_eq!(pages.len(), 1);
        let page = &pages[0];
        // The last line of the customer block bounds the header vertically:
        // the page footer below it also names the org, but it is not a column.
        let header_bottom = y_of(page, "Freedonia").expect("last header line");
        let left_end = MARGIN + COLUMN_W;
        let right_start = MARGIN + COLUMN_W + COLUMN_GAP;
        for op in page {
            if let Op::Text {
                face,
                size,
                x,
                y,
                bytes,
            } = op
            {
                // `Page N of M` and the credit are in the page footer, not a
                // body column.
                if bytes.starts_with(b"Page ") || *y > header_bottom {
                    continue;
                }
                let right = x + measure(*face, *size, bytes);
                if *x >= MARGIN && *x < left_end {
                    assert!(
                        right <= left_end,
                        "{:?} ends at {right}, crossing the left column at {left_end}",
                        String::from_utf8_lossy(bytes)
                    );
                } else if *x >= right_start {
                    assert!(
                        right <= MARGIN + CONTENT_W,
                        "{:?} ends at {right}, crossing the content edge",
                        String::from_utf8_lossy(bytes)
                    );
                }
            }
        }
        // The deliberately long issuer name and address line both wrapped, so
        // the test would fail if the header still used the full content width.
        let name_first = y_of(page, "Impossibly").expect("issuer name start");
        let name_last = y_of(page, "Column").expect("issuer name end");
        assert!(name_first < name_last, "the issuer name wraps");
        let street_first = y_of(page, "Exceedingly").expect("issuer street start");
        let street_last = y_of(page, "Well,").expect("issuer street end");
        assert!(street_first < street_last, "the issuer address line wraps");
    }

    /// The x baseline of the first text op whose glyph run is exactly `word`.
    fn x_of(page: &[Op], word: &str) -> Option<i32> {
        page.iter().find_map(|op| match op {
            Op::Text { bytes, x, .. } if bytes == word.as_bytes() => Some(*x),
            _ => None,
        })
    }

    #[test]
    fn metadata_identity_columns_totals_and_footer_follow_the_new_order() {
        let mut inv = sample_invoice();
        inv.subject = "October services".into();
        let mut o = org();
        o.legal_id = "BE 0123 4567 8901".into();
        let pages = layout(&doc_for(&inv, &customer(), &o));
        let page = &pages[0];
        let meta = y_of(page, "Invoice ID:").expect("invoice id row");
        let from = y_of(page, "From").expect("From heading");
        let invoice_for = y_of(page, "for").expect("Invoice for heading");
        assert!(meta < from, "invoice metadata sits above From");
        assert_eq!(from, invoice_for, "the identity columns start aligned");
        let invoice_for_x = page
            .iter()
            .find_map(|op| match op {
                Op::Text { x, y, bytes, .. } if *y == invoice_for && bytes == b"Invoice" => {
                    Some(*x)
                }
                _ => None,
            })
            .expect("Invoice for heading start");
        assert!(
            x_of(page, "From").unwrap() < invoice_for_x,
            "From is the left column and Invoice for the right"
        );

        let tax = y_of(page, "Tax ID:").expect("Tax ID row");
        let tax_value = page
            .iter()
            .find_map(|op| match op {
                Op::Text { x, y, bytes, .. } if *y == tax && bytes.starts_with(b"BE") => Some(*x),
                _ => None,
            })
            .expect("Tax ID value");
        assert!(
            tax_value - x_of(page, "Tax ID:").unwrap() < 80,
            "the Tax ID value stays close to its label"
        );

        let value_right = MARGIN + COL_ITEM + COL_DESC + COL_QTY + COL_UNIT + COL_AMOUNT - 4;
        let label_right = MARGIN + COL_ITEM + COL_DESC + COL_QTY + COL_UNIT - 8;
        let (total_label_right, total_y) = page
            .iter()
            .find_map(|op| match op {
                Op::TextRight {
                    x_right, y, bytes, ..
                } if bytes == b"Total:" => Some((*x_right, *y)),
                _ => None,
            })
            .expect("Total row");
        assert_eq!(total_label_right, label_right);
        assert!(
            page.iter().any(|op| matches!(
                op,
                Op::TextRight { x_right, y, .. } if *y == total_y && *x_right == value_right
            )),
            "the Total value is right-aligned under the Amount column"
        );

        assert!(
            page.iter().any(|op| matches!(
                op,
                Op::Text { x, y, bytes, .. }
                    if *x == MARGIN && *y == FOOTER_Y && bytes.starts_with(b"Generated by")
            )),
            "the generator credit is in the page footer"
        );
    }

    #[test]
    fn subject_subtitle_sits_under_the_title_with_one_blank_line() {
        let mut inv = sample_invoice();
        inv.subject = "October services".into();
        let pages = layout(&doc_for(&inv, &customer(), &org()));
        let page = &pages[0];
        let title = y_of(page, "INVOICE").expect("title");
        let subject = y_of(page, "Subject:").expect("subject label");
        assert_eq!(
            subject - title,
            23,
            "one 13pt blank line plus the 10pt body"
        );
        // The value starts just after the label, not in the 110pt metadata column.
        let value = page
            .iter()
            .find_map(|op| match op {
                Op::Text { x, bytes, y, .. } if *y == subject && bytes.starts_with(b"October") => {
                    Some(*x)
                }
                _ => None,
            })
            .expect("subject value");
        assert!(
            value < MARGIN + 80,
            "value {value} stays close to the label"
        );
    }

    #[test]
    fn long_subject_uses_the_full_width_instead_of_wrapping_into_the_columns() {
        let subject = "October services across several projects and additional work for the month";
        let label = measure(Face::Bold, 10, b"Subject:") + 6;
        let width = measure(Face::Regular, 10, &winansi(subject));
        assert!(
            width > COLUMN_W - label,
            "the fixture must exceed the old left-column width ({width}pt)"
        );
        assert!(
            width <= CONTENT_W - label,
            "the fixture must fit the full content width ({width}pt)"
        );
        let mut inv = sample_invoice();
        inv.subject = subject.into();
        let pages = layout(&doc_for(&inv, &customer(), &org()));
        let page = &pages[0];
        let subject_y = y_of(page, "Subject:").expect("subject label");
        assert_eq!(
            y_of(page, "additional"),
            Some(subject_y),
            "the subject continues on the label's line"
        );
    }

    #[test]
    fn spans_carry_bold_and_italic_faces() {
        let doc = Doc {
            title: "T".into(),
            subject: None,
            header: vec![],
            meta: vec![],
            column_labels: Default::default(),
            total_hours: None,
            accent: None,
            watermark: None,
            blocks: vec![Block::Paragraph(vec![
                Span::bold("B"),
                Span::text(" "),
                Span::italic("I"),
                Span::text(" plain"),
            ])],
            lines: vec![],
            totals: vec![],
            footer: vec![],
            credit: "Generated by Test on TucanoTime.".into(),
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
        many_lines(&mut inv, 80);
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

    #[test]
    fn single_page_document_is_numbered_one_of_one() {
        let doc = doc_for(&sample_invoice(), &customer(), &org());
        let pages = layout(&doc);
        assert_eq!(pages.len(), 1);
        assert_eq!(page_footer(&pages[0]).as_deref(), Some("Page 1 of 1"));
    }
}
