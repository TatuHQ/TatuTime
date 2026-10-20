use super::layout::{Face, Op, PAGE_H, PAGE_W, WATERMARK_GREY, layout};
use super::metrics::{measure, width_units};
use super::model::Doc;

/// Render the document to PDF bytes. Pure: same `Doc` in, byte-identical
/// output out — see the module determinism contract.
#[must_use]
pub fn render_invoice_pdf(doc: &Doc) -> Vec<u8> {
    let pages = layout(doc);
    emit(&pages)
}

pub fn without_delivery_watermark(bytes: &[u8]) -> Result<Vec<u8>, &'static str> {
    use lopdf::{Document, Object, content::Content};
    let mut document = Document::load_mem(bytes).map_err(|_| "invoice PDF could not be read")?;
    let number = |object: &Object| match object {
        Object::Integer(value) => *value as f32,
        Object::Real(value) => *value,
        _ => f32::NAN,
    };
    let mut changed = false;
    for page in document.get_pages().into_values() {
        let bytes = document.get_page_content(page);
        let mut content =
            Content::decode(&bytes).map_err(|_| "invoice PDF content could not be read")?;
        let ops = &content.operations;
        if ops.len() < 7 {
            continue;
        }
        let operators: Vec<_> = ops[..7].iter().map(|op| op.operator.as_str()).collect();
        let text = ops[4]
            .operands
            .first()
            .and_then(|value| value.as_str().ok());
        let stamp = text.is_some_and(|value| {
            [b"DRAFT".as_slice(), b"SENT".as_slice(), b"PAID".as_slice()].contains(&value)
        });
        let rotated = ops[3].operands.len() == 6
            && [0.70710677, 0.70710677, -0.70710677, 0.70710677]
                .iter()
                .zip(&ops[3].operands)
                .all(|(expected, actual)| (number(actual) - expected).abs() < 0.0001);
        let known_font = ops[2].operands.len() == 2
            && ops[2].operands[0].as_name().is_ok_and(|name| name == b"F2")
            && number(&ops[2].operands[1]) == 60.0;
        let known_ink = ops[0]
            .operands
            .first()
            .is_some_and(|value| number(value) == WATERMARK_GREY)
            && ops[6]
                .operands
                .first()
                .is_some_and(|value| number(value) == 0.0);
        if operators == ["g", "BT", "Tf", "Tm", "Tj", "ET", "g"]
            && stamp
            && rotated
            && known_font
            && known_ink
        {
            content.operations.drain(..7);
            let data = content
                .encode()
                .map_err(|_| "invoice PDF delivery content could not be encoded")?;
            document
                .change_page_content(page, data)
                .map_err(|_| "invoice PDF delivery page could not be prepared")?;
            changed = true;
        }
    }
    if !changed {
        return Ok(bytes.to_vec());
    }
    let mut output = Vec::new();
    document
        .save_to(&mut output)
        .map_err(|_| "invoice PDF delivery could not be prepared")?;
    Ok(output)
}

/// Turn laid-out pages into a PDF 1.4 file. Object ids are fixed:
/// 1 catalog, 2 page tree (carrying the shared /Resources), 3 resources,
/// 4..7 fonts, 8+ page/content pairs. No /Info, no file ID — those carry
/// timestamps and would break byte determinism.
fn emit(pages: &[Vec<Op>]) -> Vec<u8> {
    use pdf_writer::{Content, Name, Pdf, Rect, Ref, writers::Resources};

    let catalog = Ref::new(1);
    let page_tree = Ref::new(2);
    let resources = Ref::new(3);
    let faces = [Face::Regular, Face::Bold, Face::Oblique, Face::BoldOblique];
    let font_ids: Vec<Ref> = (4..8).map(Ref::new).collect();
    let first_page = 8usize;

    let mut pdf = Pdf::new();
    pdf.set_version(1, 4);
    pdf.catalog(catalog).pages(page_tree);
    let page_refs: Vec<Ref> = (0..pages.len())
        .map(|i| Ref::new((first_page + 2 * i) as i32))
        .collect();
    {
        let mut pw = pdf.pages(page_tree);
        pw.kids(page_refs.iter().copied()).count(pages.len() as i32);
        // /Resources is inheritable from the page tree (PDF 1.4 §7.3.1), so
        // the font dict is written once for every page.
        pw.pair(Name(b"Resources"), resources);
    }

    {
        let mut res = pdf.indirect(resources).start::<Resources>();
        let mut fonts = res.fonts();
        for (id, face) in font_ids.iter().zip(faces) {
            fonts.pair(Name(face.name().as_bytes()), *id);
        }
    }

    for (id, face) in font_ids.iter().zip(faces) {
        let mut f = pdf.type1_font(*id);
        f.base_font(Name(face.base()))
            .encoding_predefined(Name(b"WinAnsiEncoding"))
            .first_char(32)
            .last_char(255)
            .widths((32..=255u8).map(|c| width_units(face, c) as f32));
    }

    for (i, ops) in pages.iter().enumerate() {
        let content_ref = Ref::new((first_page + 2 * i + 1) as i32);
        let mut content = Content::new();
        for op in ops {
            match op {
                Op::Color { rgb } => {
                    let [r, g, b] = *rgb;
                    content.set_fill_rgb(r, g, b);
                    content.set_stroke_rgb(r, g, b);
                }
                Op::Text {
                    face,
                    size,
                    x,
                    y,
                    bytes,
                } => {
                    put_text(&mut content, *face, *size, *x, *y, bytes);
                }
                Op::TextRight {
                    face,
                    size,
                    x_right,
                    y,
                    bytes,
                } => {
                    let w = measure(*face, *size, bytes);
                    put_text(&mut content, *face, *size, *x_right - w, *y, bytes);
                }
                Op::TextRotated {
                    face,
                    size,
                    x,
                    y,
                    bytes,
                } => {
                    put_text_rotated(&mut content, *face, *size, *x, *y, bytes);
                }
                Op::Line {
                    x1,
                    y1,
                    x2,
                    y2,
                    width,
                } => {
                    content.set_line_width(*width as f32);
                    content.move_to(*x1 as f32, (PAGE_H - y1) as f32);
                    content.line_to(*x2 as f32, (PAGE_H - y2) as f32);
                    content.stroke();
                }
            }
        }
        let buf = content.finish();
        pdf.stream(content_ref, buf.as_slice());
        pdf.page(page_refs[i])
            .parent(page_tree)
            .media_box(Rect::new(0.0, 0.0, PAGE_W as f32, PAGE_H as f32))
            .contents(content_ref);
    }

    pdf.finish()
}

/// One positioned text draw; PDF's y axis points up, ours doesn't.
fn put_text(
    content: &mut pdf_writer::Content,
    face: Face,
    size: i32,
    x: i32,
    y: i32,
    bytes: &[u8],
) {
    use pdf_writer::{Name, Str};
    content.begin_text();
    content.set_font(Name(face.name().as_bytes()), size as f32);
    content.set_text_matrix([1.0, 0.0, 0.0, 1.0, x as f32, (PAGE_H - y) as f32]);
    content.show(Str(bytes));
    content.end_text();
}

/// One rotated text draw — the watermark stamp. Unlike [`put_text`], the text
/// matrix carries a 45° anticlockwise rotation: `[cos45, sin45, -sin45, cos45,
/// x, y]`. In PDF's bottom-up space the positive `b` (`sin45`) tilts the
/// baseline up and to the right, so the run reads bottom-left to top-right on
/// the page. Text-only, no date — the determinism contract.
fn put_text_rotated(
    content: &mut pdf_writer::Content,
    face: Face,
    size: i32,
    x: i32,
    y: i32,
    bytes: &[u8],
) {
    use pdf_writer::{Name, Str};
    let c = std::f32::consts::FRAC_1_SQRT_2;
    // Light grey DeviceGray ink (`g`), restored to black (`0 g`) afterwards so
    // the invoice content drawn on top stays black. DeviceGray is deliberate:
    // one grey value needs no `ExtGState`/alpha, and it can never be mistaken
    // for the RGB accent (#146) the money section may paint.
    content.set_fill_gray(WATERMARK_GREY);
    content.begin_text();
    content.set_font(Name(face.name().as_bytes()), size as f32);
    content.set_text_matrix([c, c, -c, c, x as f32, (PAGE_H - y) as f32]);
    content.show(Str(bytes));
    content.end_text();
    content.set_fill_gray(0.0);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Currency, Customer, Invoice, InvoiceLine, InvoiceStatus, LineKind};
    use crate::pdf::{Org, doc_for};
    use chrono::{TimeZone, Utc};

    #[test]
    fn delivery_removes_only_the_application_watermark() {
        let invoice = sample_invoice();
        let customer = customer();
        let mut doc = doc_for(&invoice, &customer, &org());
        doc.watermark = Some(crate::domain::InvoiceWatermarkStage::Draft);
        doc.footer.push(super::super::model::Block::Paragraph(vec![
            super::super::model::Span::text("DRAFT reference remains in normal text"),
        ]));
        let archived = render_invoice_pdf(&doc);
        let clean = without_delivery_watermark(&archived).unwrap();
        let document = lopdf::Document::load_mem(&clean).unwrap();
        let text = document
            .extract_text(&document.get_pages().keys().copied().collect::<Vec<_>>())
            .unwrap();
        assert!(
            text.split_whitespace()
                .collect::<String>()
                .contains("DRAFTreferenceremainsinnormaltext")
        );
        let raw = document.get_page_content(*document.get_pages().values().next().unwrap());
        let original = lopdf::Document::load_mem(&archived).unwrap();
        let original_page =
            original.get_page_content(*original.get_pages().values().next().unwrap());
        let original_content = lopdf::content::Content::decode(&original_page).unwrap();
        let expected = lopdf::content::Content {
            operations: original_content
                .operations
                .into_iter()
                .skip(7)
                .collect::<Vec<_>>(),
        }
        .encode()
        .unwrap();
        let normalized = lopdf::content::Content::decode(&raw)
            .unwrap()
            .encode()
            .unwrap();
        assert_eq!(
            normalized, expected,
            "all non-watermark drawing operations must remain identical"
        );
        let operations = lopdf::content::Content::decode(&raw).unwrap().operations;
        assert!(!operations.iter().any(|op| op.operator == "Tm" && op.operands.first().is_some_and(|value| matches!(value, lopdf::Object::Real(value) if (*value - 0.70710677).abs() < 0.0001))));
        assert_ne!(archived, clean);
        assert_eq!(without_delivery_watermark(&clean).unwrap(), clean);
    }

    #[test]
    fn clean_delivery_keeps_unstamped_archive_bytes_identical() {
        let doc = doc_for(&sample_invoice(), &customer(), &org());
        let bytes = render_invoice_pdf(&doc);
        assert_eq!(without_delivery_watermark(&bytes).unwrap(), bytes);
        assert!(without_delivery_watermark(b"not a PDF").is_err());
    }

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

    #[test]
    fn renderer_is_deterministic() {
        let doc = doc_for(&sample_invoice(), &customer(), &org());
        let a = render_invoice_pdf(&doc);
        let b = render_invoice_pdf(&doc);
        assert_eq!(a, b, "same Doc must produce byte-identical PDFs");
    }

    #[test]
    fn output_is_a_pdf_and_reparses() {
        let doc = doc_for(&sample_invoice(), &customer(), &org());
        let bytes = render_invoice_pdf(&doc);
        assert!(bytes.starts_with(b"%PDF-1.4"), "header");
        assert!(bytes.ends_with(b"%%EOF"), "trailer");
        // Independent validation (ADR guard 2): lopdf must parse it.
        let _ = lopdf::Document::load_mem(&bytes).expect("re-parse");
    }

    #[test]
    fn empty_note_invoice_renders() {
        let mut inv = sample_invoice();
        inv.lines.clear();
        inv.total_minor = 0;
        let doc = doc_for(&inv, &customer(), &org());
        let bytes = render_invoice_pdf(&doc);
        assert!(bytes.starts_with(b"%PDF"));
    }
}
