use std::collections::HashSet;
use std::io::{Cursor, Read};

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::harvest::decimal;

pub const MAX_ZIP_BYTES: usize = 5 * 1024 * 1024;
pub const MAX_ARCHIVE_ENTRIES: usize = 1000;
pub const MAX_INVOICES: usize = 500;
pub const MAX_EXPANDED_BYTES: u64 = 50 * 1024 * 1024;
pub const MAX_COMPRESSION_RATIO: u64 = 200;

#[derive(Debug, Serialize)]
pub struct InvoiceSource {
    pub original_name: String,
    pub sha256: String,
    pub bytes: usize,
    #[serde(skip)]
    pub pdf: Vec<u8>,
}

#[derive(Debug, thiserror::Error, Serialize)]
#[error("{message}")]
pub struct InvoiceZipError {
    pub member: Option<usize>,
    pub message: &'static str,
}

fn error(member: Option<usize>, message: &'static str) -> InvoiceZipError {
    InvoiceZipError { member, message }
}

pub fn inspect_invoice_zip(bytes: &[u8]) -> Result<Vec<InvoiceSource>, InvoiceZipError> {
    if bytes.len() > MAX_ZIP_BYTES {
        return Err(error(None, "invoice ZIP exceeds 5 MiB"));
    }
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes))
        .map_err(|_| error(None, "invalid or unsupported ZIP archive"))?;
    if archive.len() > MAX_ARCHIVE_ENTRIES {
        return Err(error(None, "invoice ZIP has too many members"));
    }
    let mut names = HashSet::new();
    let mut hashes = HashSet::new();
    let mut total = 0_u64;
    let mut invoices = Vec::new();
    for index in 0..archive.len() {
        let member = Some(index + 1);
        let mut file = archive
            .by_index(index)
            .map_err(|_| error(member, "unreadable or encrypted ZIP member"))?;
        let name = std::str::from_utf8(file.name_raw())
            .map_err(|_| error(member, "ZIP member name must be UTF-8"))?
            .to_owned();
        if name.len() > 240
            || name.contains(['\\', ':'])
            || name.chars().any(char::is_control)
            || file.enclosed_name().is_none()
            || name
                .trim_end_matches('/')
                .split('/')
                .any(|part| part.is_empty() || part == "." || part == "..")
        {
            return Err(error(member, "unsafe ZIP member path"));
        }
        if !names.insert(name.trim_end_matches('/').to_lowercase()) {
            return Err(error(member, "duplicate ZIP member path"));
        }
        let mode = file.unix_mode().unwrap_or(0) & 0o170000;
        if file.encrypted() || file.is_symlink() || !matches!(mode, 0 | 0o100000 | 0o040000) {
            return Err(error(
                member,
                "encrypted or non-regular ZIP members are not supported",
            ));
        }
        if file.is_dir() {
            if file.size() != 0 {
                return Err(error(member, "ZIP directories must be empty"));
            }
            continue;
        }
        if mode == 0o040000 || !name.to_ascii_lowercase().ends_with(".pdf") {
            return Err(error(
                member,
                "invoice ZIP may contain only PDF files and directories",
            ));
        }
        if invoices.len() == MAX_INVOICES {
            return Err(error(member, "invoice ZIP exceeds 500 PDFs"));
        }
        let declared = file.size();
        let compressed = file.compressed_size();
        if declared > crate::store::MAX_PDF_BYTES
            || total.saturating_add(declared) > MAX_EXPANDED_BYTES
            || declared > compressed.saturating_mul(MAX_COMPRESSION_RATIO)
        {
            return Err(error(
                member,
                "ZIP member exceeds expanded size or compression limits",
            ));
        }
        let mut pdf = Vec::new();
        let limit = crate::store::MAX_PDF_BYTES.min(MAX_EXPANDED_BYTES.saturating_sub(total));
        (&mut file)
            .take(limit + 1)
            .read_to_end(&mut pdf)
            .map_err(|_| {
                error(
                    member,
                    "ZIP member failed decompression or CRC verification",
                )
            })?;
        let actual = pdf.len() as u64;
        if actual != declared
            || actual > limit
            || actual > compressed.saturating_mul(MAX_COMPRESSION_RATIO)
        {
            return Err(error(member, "ZIP member actual size exceeds its limits"));
        }
        if !pdf.starts_with(b"%PDF-")
            || !pdf[pdf.len().saturating_sub(1024)..]
                .windows(5)
                .any(|marker| marker == b"%%EOF")
        {
            return Err(error(member, "ZIP member is not a complete PDF document"));
        }
        let sha256 = crate::hex_lower(&Sha256::digest(&pdf));
        if !hashes.insert(sha256.clone()) {
            return Err(error(member, "duplicate PDF contents require review"));
        }
        total += actual;
        invoices.push(InvoiceSource {
            original_name: name,
            sha256,
            bytes: pdf.len(),
            pdf,
        });
    }
    if invoices.is_empty() {
        return Err(error(None, "invoice ZIP contains no PDFs"));
    }
    Ok(invoices)
}

/// Hard ceilings for the best-effort PDF text pass (#399). Parsing happens in
/// memory over an archive member already capped at 8 MiB; these bounds keep a
/// decompression bomb or a pathological layout from turning a preview into an
/// unbounded allocation. They are *safety* limits, not a claim of full PDF
/// support: anything outside them becomes a blocking manual-review result.
pub const MAX_PDF_PAGES: usize = 50;
pub const MAX_PDF_PAGE_STREAM_BYTES: usize = 1024 * 1024;
pub const MAX_PDF_TEXT_BYTES: usize = 256 * 1024;
const MAX_LINES_PER_PAGE: usize = 2_000;
const MAX_EVIDENCE_CHARS: usize = 160;
const MAX_VALUE_CHARS: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PdfField {
    InvoiceNumber,
    InvoiceDate,
    Customer,
    Currency,
    Total,
    AmountDue,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PdfConfidence {
    Low,
    Medium,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PdfSuggestion {
    pub field: PdfField,
    /// The value exactly as it appears in the document, trimmed and bounded.
    pub value: String,
    /// Parsed integer minor units for monetary fields; never a float.
    pub amount_minor: Option<i64>,
    /// 1-based page the evidence was read from.
    pub page: u32,
    /// The source line the suggestion was drawn from, for manual review.
    pub evidence: String,
    pub confidence: PdfConfidence,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PdfInspectionStatus {
    /// At least one field was suggested; every one still needs review.
    Suggestions,
    /// The document parsed but has no extractable text (a scan, typically).
    NoText,
    /// The document could not be inspected safely or completely: block on
    /// manual resolution or explicit exclusion.
    ManualReview,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PdfInspection {
    pub pages: usize,
    pub text_bytes: usize,
    pub text_truncated: bool,
    pub status: PdfInspectionStatus,
    /// Why a `ManualReview` is required. Never a substitute for the evidence.
    pub reason: Option<&'static str>,
    /// Suggestions only; the original PDF stays authoritative and nothing here
    /// may be committed without explicit review (#399/#400).
    pub suggestions: Vec<PdfSuggestion>,
}

fn manual(reason: &'static str, suggestions: Vec<PdfSuggestion>, pages: usize) -> PdfInspection {
    PdfInspection {
        pages,
        text_bytes: 0,
        text_truncated: false,
        status: PdfInspectionStatus::ManualReview,
        reason: Some(reason),
        suggestions,
    }
}

fn head(text: &str, max: usize) -> &str {
    match text.char_indices().nth(max) {
        Some((index, _)) => &text[..index],
        None => text,
    }
}

fn normalise_label(raw: &str) -> String {
    raw.trim()
        .to_lowercase()
        .replace('#', " # ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn field_for_label(label: &str) -> Option<PdfField> {
    match label {
        "invoice number" | "invoice no" | "invoice no." | "invoice nr" | "invoice nr."
        | "invoice #" | "invoice" | "invoice ref" | "invoice reference" => {
            Some(PdfField::InvoiceNumber)
        }
        "invoice date" | "date" | "issue date" | "issued" | "issued on" => {
            Some(PdfField::InvoiceDate)
        }
        "bill to" | "billed to" | "customer" | "client" => Some(PdfField::Customer),
        "currency" => Some(PdfField::Currency),
        "total" | "invoice total" | "grand total" | "total amount" => Some(PdfField::Total),
        "amount due" | "balance due" | "total due" | "amount payable" => Some(PdfField::AmountDue),
        _ => None,
    }
}

/// Parse a displayed amount into integer minor units, optionally recovering a
/// currency token printed before or after it. Grouped thousands are accepted
/// only with correct separators; parentheses denote a credit. Anything else is
/// left for manual review instead of being guessed.
fn parse_amount(raw: &str) -> Option<(i64, Option<crate::domain::Currency>)> {
    let mut text = raw.trim();
    let mut currency = None;
    if let Some((head, tail)) = text.rsplit_once(char::is_whitespace)
        && let Some(parsed) = crate::domain::Currency::parse(tail)
    {
        currency = Some(parsed);
        text = head.trim();
    }
    if currency.is_none()
        && let Some((head, tail)) = text.split_once(char::is_whitespace)
        && let Some(parsed) = crate::domain::Currency::parse(head)
    {
        currency = Some(parsed);
        text = tail.trim();
    }
    loop {
        let stripped = text.trim_start_matches(|character: char| {
            character.is_whitespace() || matches!(character, '$' | '€' | '£' | '¥')
        });
        if stripped == text {
            break;
        }
        text = stripped;
    }
    let parenthesised = text.starts_with('(') && text.ends_with(')');
    if parenthesised {
        text = &text[1..text.len() - 1];
    }
    let mut amount = decimal(text, true)?;
    if parenthesised && amount > 0 {
        amount = -amount;
    }
    Some((amount, currency))
}

fn offer(
    suggestions: &mut Vec<PdfSuggestion>,
    field: PdfField,
    value: &str,
    amount_minor: Option<i64>,
    page: u32,
    evidence: &str,
    confidence: PdfConfidence,
) {
    let value = head(value.trim(), MAX_VALUE_CHARS).trim();
    if value.is_empty() {
        return;
    }
    suggestions.push(PdfSuggestion {
        field,
        value: value.to_string(),
        amount_minor,
        page,
        evidence: head(evidence.trim(), MAX_EVIDENCE_CHARS).to_string(),
        confidence,
    });
}

fn scan_page(page: u32, text: &str, suggestions: &mut Vec<PdfSuggestion>) {
    for line in text.lines().take(MAX_LINES_PER_PAGE) {
        let Some((raw_label, raw_value)) = line.split_once(':') else {
            continue;
        };
        let label = normalise_label(raw_label);
        let Some(field) = field_for_label(&label) else {
            continue;
        };
        let value = raw_value.trim();
        match field {
            PdfField::InvoiceNumber => {
                let value = value.trim_start_matches('#').trim();
                let valid = !value.is_empty()
                    && value.len() <= MAX_VALUE_CHARS
                    && value.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'/' | b'.')
                    });
                if valid {
                    offer(
                        suggestions,
                        field,
                        value,
                        None,
                        page,
                        line,
                        PdfConfidence::Medium,
                    );
                }
            }
            PdfField::InvoiceDate => {
                if value.len() == 10 && chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d").is_ok()
                {
                    offer(
                        suggestions,
                        field,
                        value,
                        None,
                        page,
                        line,
                        PdfConfidence::Medium,
                    );
                }
            }
            PdfField::Customer => {
                if !value.is_empty()
                    && value.chars().count() <= 120
                    && !value.chars().any(char::is_control)
                {
                    offer(
                        suggestions,
                        field,
                        value,
                        None,
                        page,
                        line,
                        PdfConfidence::Low,
                    );
                }
            }
            PdfField::Currency => {
                if let Some(currency) = crate::domain::Currency::parse(value) {
                    offer(
                        suggestions,
                        field,
                        &currency.0,
                        None,
                        page,
                        line,
                        PdfConfidence::Medium,
                    );
                }
            }
            PdfField::Total | PdfField::AmountDue => {
                if let Some((amount, currency)) = parse_amount(value) {
                    offer(
                        suggestions,
                        field,
                        value,
                        Some(amount),
                        page,
                        line,
                        PdfConfidence::Medium,
                    );
                    if let Some(currency) = currency {
                        offer(
                            suggestions,
                            PdfField::Currency,
                            &currency.0,
                            None,
                            page,
                            line,
                            PdfConfidence::Low,
                        );
                    }
                }
            }
        }
    }
}

fn dedupe(suggestions: &mut Vec<PdfSuggestion>) {
    let mut seen = HashSet::new();
    suggestions.retain(|suggestion| {
        seen.insert((suggestion.field, suggestion.value.clone(), suggestion.page))
    });
}

/// Best-effort metadata suggestions from one already-validated ZIP member.
///
/// The pass is bounded (page count, per-page decompressed content, total text)
/// and never performs OCR, network access or filename inference. A scanned,
/// encrypted or unsupported document returns `ManualReview` rather than an
/// empty success, so the preview cannot silently treat it as reviewed (#399).
#[must_use]
pub fn inspect_invoice_text(source: &InvoiceSource) -> PdfInspection {
    let document = match lopdf::Document::load_mem_with_options(
        &source.pdf,
        lopdf::LoadOptions::with_max_decompressed_size(MAX_PDF_PAGE_STREAM_BYTES),
    ) {
        Ok(document) => document,
        Err(_) => {
            return manual(
                "PDF structure could not be read within the configured limits",
                Vec::new(),
                0,
            );
        }
    };
    if document.is_encrypted() {
        return manual("encrypted PDFs require manual resolution", Vec::new(), 0);
    }
    let pages = document.get_pages();
    if pages.is_empty() {
        return manual("PDF contains no pages", Vec::new(), 0);
    }
    if pages.len() > MAX_PDF_PAGES {
        return manual(
            "PDF has more pages than the bounded inspector accepts",
            Vec::new(),
            pages.len(),
        );
    }
    let mut suggestions = Vec::new();
    let mut text_bytes = 0_usize;
    let mut text_truncated = false;
    let mut saw_text = false;
    let mut failed = false;
    for number in pages.keys().copied() {
        if text_bytes >= MAX_PDF_TEXT_BYTES {
            text_truncated = true;
            break;
        }
        match document.extract_text_with_limit(&[number], MAX_PDF_PAGE_STREAM_BYTES) {
            Ok(text) => {
                saw_text |= text.chars().any(|character| !character.is_whitespace());
                let remaining = MAX_PDF_TEXT_BYTES.saturating_sub(text_bytes);
                let slice = if text.len() > remaining {
                    text_truncated = true;
                    head(&text, remaining)
                } else {
                    text.as_str()
                };
                text_bytes += slice.len();
                scan_page(number, slice, &mut suggestions);
            }
            Err(_) => failed = true,
        }
    }
    dedupe(&mut suggestions);
    let status = if failed {
        PdfInspectionStatus::ManualReview
    } else if suggestions.is_empty() {
        if saw_text {
            PdfInspectionStatus::ManualReview
        } else {
            PdfInspectionStatus::NoText
        }
    } else {
        PdfInspectionStatus::Suggestions
    };
    let reason = match status {
        PdfInspectionStatus::Suggestions => None,
        PdfInspectionStatus::NoText => {
            Some("PDF contains no extractable text (possibly a scan); manual review required")
        }
        PdfInspectionStatus::ManualReview if failed => {
            Some("page content could not be decoded within the configured limits")
        }
        PdfInspectionStatus::ManualReview => {
            Some("no invoice metadata could be identified; manual review required")
        }
    };
    PdfInspection {
        pages: pages.len(),
        text_bytes,
        text_truncated,
        status,
        reason,
        suggestions,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    fn archive(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, data) in files {
            writer
                .start_file(
                    *name,
                    SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored),
                )
                .unwrap();
            writer.write_all(data).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    const PDF: &[u8] = b"%PDF-1.7\noriginal invoice\n%%EOF\n";

    /// Minimal, well-formed PDF with one text line per entry, built without a
    /// dependency on this project's invoice renderer so the inspector is tested
    /// against foreign layout bytes.
    fn pdf_document(pages: &[&[&str]]) -> Vec<u8> {
        fn escape(text: &str) -> String {
            text.replace('\\', "\\\\")
                .replace('(', "\\(")
                .replace(')', "\\)")
        }
        let mut objects = Vec::new();
        let kids = (0..pages.len())
            .map(|index| format!("{} 0 R", 4 + index * 2))
            .collect::<Vec<_>>()
            .join(" ");
        objects.push("<< /Type /Catalog /Pages 2 0 R >>".to_string());
        objects.push(format!(
            "<< /Type /Pages /Kids [{kids}] /Count {} >>",
            pages.len()
        ));
        objects.push("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string());
        for (index, lines) in pages.iter().enumerate() {
            let page = 4 + index * 2;
            objects.push(format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 3 0 R >> >> /Contents {} 0 R >>",
                page + 1
            ));
            let mut stream = String::new();
            for (line, text) in lines.iter().enumerate() {
                stream.push_str(&format!(
                    "BT /F1 12 Tf 72 {} Td ({}) Tj ET\n",
                    720 - line * 16,
                    escape(text)
                ));
            }
            objects.push(format!(
                "<< /Length {} >>\nstream\n{}\nendstream",
                stream.len(),
                stream
            ));
        }
        let mut out = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (index, body) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n{body}\nendobj\n", index + 1).as_bytes());
        }
        let xref = out.len();
        out.extend_from_slice(
            format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
        );
        for offset in offsets {
            out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                objects.len() + 1
            )
            .as_bytes(),
        );
        out
    }

    fn inspect(pdf: &[u8]) -> PdfInspection {
        let zip = archive(&[("H-999.pdf", pdf)]);
        let sources = inspect_invoice_zip(&zip).unwrap();
        inspect_invoice_text(&sources[0])
    }

    #[test]
    fn suggests_reviewable_metadata_from_pdf_text_without_filename_inference() {
        let pdf = pdf_document(&[&[
            "Invoice Number: H-2025-001",
            "Invoice Date: 2025-01-06",
            "Bill To: Example Ltd",
            "Currency: USD",
            "Subtotal: 1,000.00",
            "Total: 1,234.56 USD",
            "Amount Due: 1,234.56",
        ]]);
        let inspection = inspect(&pdf);
        assert_eq!(inspection.status, PdfInspectionStatus::Suggestions);
        assert_eq!(inspection.pages, 1);
        assert_eq!(inspection.reason, None);
        assert!(!inspection.text_truncated);
        assert!(
            inspection.suggestions.iter().all(|s| s.page == 1),
            "{:?}",
            inspection.suggestions
        );
        let find = |field| {
            inspection
                .suggestions
                .iter()
                .find(|suggestion| suggestion.field == field)
                .unwrap_or_else(|| panic!("missing {field:?}: {:?}", inspection.suggestions))
        };
        assert_eq!(find(PdfField::InvoiceNumber).value, "H-2025-001");
        assert_eq!(find(PdfField::InvoiceDate).value, "2025-01-06");
        assert_eq!(find(PdfField::Customer).value, "Example Ltd");
        assert_eq!(find(PdfField::Customer).confidence, PdfConfidence::Low);
        assert_eq!(find(PdfField::Currency).value, "USD");
        assert_eq!(find(PdfField::Total).amount_minor, Some(123_456));
        assert_eq!(find(PdfField::AmountDue).amount_minor, Some(123_456));
        assert!(
            inspection
                .suggestions
                .iter()
                .all(|suggestion| suggestion.value != "H-999"),
            "the filename is not an identity source"
        );
    }

    #[test]
    fn does_not_guess_dates_totals_or_customers_without_evidence() {
        let pdf = pdf_document(&[&[
            "Invoice Date: 06/01/2025",
            "Total: about a thousand",
            "Bill To:",
        ]]);
        let inspection = inspect(&pdf);
        assert_eq!(inspection.status, PdfInspectionStatus::ManualReview);
        assert!(inspection.reason.is_some());
        assert!(
            inspection.suggestions.is_empty(),
            "{:?}",
            inspection.suggestions
        );
    }

    #[test]
    fn reports_scanned_encrypted_like_and_oversized_documents_for_manual_review() {
        let scanned = inspect(&pdf_document(&[&[]]));
        assert_eq!(scanned.status, PdfInspectionStatus::NoText);
        assert!(scanned.reason.unwrap().contains("no extractable text"));

        let unreadable = inspect(b"%PDF-1.7\nthis is not a document body\n%%EOF\n");
        assert_eq!(unreadable.status, PdfInspectionStatus::ManualReview);
        assert!(unreadable.reason.is_some());

        let pages: Vec<Vec<&str>> = (0..=MAX_PDF_PAGES).map(|_| Vec::new()).collect();
        let refs: Vec<&[&str]> = pages.iter().map(Vec::as_slice).collect();
        let oversized = inspect(&pdf_document(&refs));
        assert_eq!(oversized.status, PdfInspectionStatus::ManualReview);
        assert_eq!(oversized.pages, MAX_PDF_PAGES + 1);
        assert!(oversized.reason.unwrap().contains("more pages"));
    }

    #[test]
    fn preserves_original_bytes_and_exposes_only_metadata_on_serialization() {
        let zip = archive(&[("invoices/original.pdf", PDF)]);
        let result = inspect_invoice_zip(&zip).unwrap();
        assert_eq!(result[0].pdf, PDF);
        assert_eq!(result[0].sha256, crate::hex_lower(&Sha256::digest(PDF)));
        assert!(
            serde_json::to_value(&result[0])
                .unwrap()
                .get("pdf")
                .is_none()
        );
    }

    #[test]
    fn rejects_unsafe_paths_non_pdf_files_and_duplicates() {
        for name in [
            "../invoice.pdf",
            "/invoice.pdf",
            "C:/invoice.pdf",
            "a\\invoice.pdf",
            "a/../invoice.pdf",
            "a//invoice.pdf",
            "nested.zip",
        ] {
            assert!(
                inspect_invoice_zip(&archive(&[(name, PDF)])).is_err(),
                "{name}"
            );
        }
        assert!(inspect_invoice_zip(&archive(&[("a.pdf", PDF), ("b.pdf", PDF)])).is_err());
        assert!(
            inspect_invoice_zip(&archive(&[
                ("a.pdf", PDF),
                ("A.pdf", b"%PDF-1.7\nother\n%%EOF")
            ]))
            .is_err()
        );
    }

    #[test]
    fn rejects_invalid_zip_invalid_pdf_and_oversized_uploads() {
        assert!(inspect_invoice_zip(b"not a zip").is_err());
        assert!(inspect_invoice_zip(&archive(&[])).is_err());
        assert!(inspect_invoice_zip(&archive(&[("a.pdf", b"not a PDF")])).is_err());
        assert!(inspect_invoice_zip(&vec![0; MAX_ZIP_BYTES + 1]).is_err());
    }

    #[test]
    fn rejects_member_exceeding_expanded_limit_before_reading() {
        let data = vec![b' '; crate::store::MAX_PDF_BYTES as usize + 1];
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        writer
            .start_file(
                "large.pdf",
                SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated),
            )
            .unwrap();
        writer.write_all(&data).unwrap();
        let bytes = writer.finish().unwrap().into_inner();
        assert!(bytes.len() < MAX_ZIP_BYTES);
        assert!(
            inspect_invoice_zip(&bytes)
                .unwrap_err()
                .message
                .contains("limits")
        );
    }

    #[test]
    #[ignore = "requires a private archive supplied through HARVEST_INVOICE_ZIP"]
    fn private_invoice_archive_is_inspected_without_extraction() {
        let path = std::env::var("HARVEST_INVOICE_ZIP").expect("set HARVEST_INVOICE_ZIP");
        let bytes = std::fs::read(path).unwrap();
        let invoices = inspect_invoice_zip(&bytes).unwrap();
        eprintln!(
            "invoice ZIP: {} PDFs, {} expanded bytes",
            invoices.len(),
            invoices.iter().map(|invoice| invoice.bytes).sum::<usize>()
        );
        assert!(!invoices.is_empty());
    }
}
