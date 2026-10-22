use std::collections::HashSet;
use std::io::{Cursor, Read};

use chrono::{Datelike, NaiveDate, Weekday};
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
    /// The printed invoice total.
    Total,
    /// The pre-tax/total row Harvest prints (`Subtotal`). Kept distinct from
    /// `Total` so an exporter that prints both never collapses them.
    Subtotal,
    AmountDue,
    /// The payments applied to the invoice. Printed as a credit, so the
    /// suggestion's `amount_minor` is the positive amount already paid.
    Payments,
    /// Derived once by comparing `Payments` with `Total`: `paid` when they are
    /// equal, `issued` otherwise. Never read from a word in the document.
    Status,
    /// Derived from the issue date: the calendar month before it, or its own
    /// month when the invoice is issued on the last day. Never read from a
    /// word in the document.
    PeriodFrom,
    PeriodTo,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PdfConfidence {
    Low,
    Medium,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
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
        | "invoice #" | "invoice" | "invoice ref" | "invoice reference" | "invoice id" => {
            Some(PdfField::InvoiceNumber)
        }
        "invoice date" | "date" | "issue date" | "issued" | "issued on" => {
            Some(PdfField::InvoiceDate)
        }
        "bill to" | "billed to" | "invoice for" | "invoice to" | "customer" | "client" => {
            Some(PdfField::Customer)
        }
        "currency" => Some(PdfField::Currency),
        "total" | "invoice total" | "grand total" | "total amount" => Some(PdfField::Total),
        "subtotal" | "sub total" => Some(PdfField::Subtotal),
        "payments" | "payment" | "amount paid" | "payments applied" => Some(PdfField::Payments),
        "amount due" | "balance due" | "total due" | "amount payable" => Some(PdfField::AmountDue),
        _ => None,
    }
}

/// A label printed alone on its line, with the value on the next non-empty
/// line, as PDF text extraction often yields. The bare document title
/// `INVOICE` is not an invoice-number label — only the colon form may use it.
fn standalone_label(label: &str) -> Option<PdfField> {
    if label == "invoice" {
        None
    } else {
        field_for_label(label)
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
    let parenthesised = text.starts_with('(') && text.ends_with(')');
    if parenthesised {
        text = &text[1..text.len() - 1];
    }
    // A printed credit can put its sign before the currency symbol
    // (`EUR -€5,760.00`), so strip whitespace, symbols and a leading sign in
    // any order before delegating the digits to the shared decimal parser.
    let mut body = text;
    let mut sign = 1_i64;
    loop {
        let trimmed = body.trim_start();
        let stripped = trimmed.trim_start_matches(['$', '€', '£', '¥']);
        if let Some(rest) = stripped.strip_prefix('-') {
            sign = -sign;
            body = rest;
            continue;
        }
        if let Some(rest) = stripped.strip_prefix('+') {
            body = rest;
            continue;
        }
        let removed_symbol = stripped.len() != trimmed.len();
        body = stripped;
        if !removed_symbol {
            break;
        }
    }
    let mut amount = decimal(body.trim(), true)?.checked_mul(sign)?;
    if parenthesised && amount > 0 {
        amount = -amount;
    }
    Some((amount, currency))
}

/// Dates are accepted only in the forms the source documents print. Harvest
/// prints the issue date as `DD/MM/YYYY`; ISO stays accepted for other
/// exporters. The suggestion is normalised to ISO so the review field can be a
/// plain `date` input.
fn parse_invoice_date(value: &str) -> Option<NaiveDate> {
    ["%Y-%m-%d", "%d/%m/%Y", "%d.%m.%Y"]
        .iter()
        .find_map(|format| NaiveDate::parse_from_str(value.trim(), format).ok())
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

fn suggest(
    page: u32,
    field: PdfField,
    value: &str,
    evidence: &str,
    suggestions: &mut Vec<PdfSuggestion>,
) {
    let value = value.trim();
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
                    evidence,
                    PdfConfidence::Medium,
                );
            }
        }
        PdfField::InvoiceDate => {
            if let Some(date) = parse_invoice_date(value) {
                offer(
                    suggestions,
                    field,
                    &date.format("%Y-%m-%d").to_string(),
                    None,
                    page,
                    evidence,
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
                    evidence,
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
                    evidence,
                    PdfConfidence::Medium,
                );
            }
        }
        PdfField::Total | PdfField::Subtotal | PdfField::AmountDue | PdfField::Payments => {
            if let Some((amount, currency)) = parse_amount(value) {
                // Payments are printed as a credit; suggest the positive
                // amount already paid so the review field can be prefilled.
                let suggested = if field == PdfField::Payments {
                    amount.checked_abs()
                } else {
                    Some(amount)
                };
                offer(
                    suggestions,
                    field,
                    value,
                    suggested,
                    page,
                    evidence,
                    PdfConfidence::Medium,
                );
                if let Some(currency) = currency {
                    offer(
                        suggestions,
                        PdfField::Currency,
                        &currency.0,
                        None,
                        page,
                        evidence,
                        PdfConfidence::Low,
                    );
                }
            }
        }
        PdfField::Status | PdfField::PeriodFrom | PdfField::PeriodTo => {}
    }
}

fn month_bounds(year: i32, month: u32) -> Option<(NaiveDate, NaiveDate)> {
    let start = NaiveDate::from_ymd_opt(year, month, 1)?;
    let (next_year, next_month) = if month == 12 {
        (year.checked_add(1)?, 1)
    } else {
        (year, month + 1)
    };
    let next = NaiveDate::from_ymd_opt(next_year, next_month, 1)?;
    Some((start, next.pred_opt()?))
}

/// The last weekday of a month. Source invoices are normally issued on it, so
/// a weekend date at the very end of the month still belongs to that month.
fn last_weekday(year: i32, month: u32) -> Option<NaiveDate> {
    let (_, mut date) = month_bounds(year, month)?;
    while matches!(date.weekday(), Weekday::Sat | Weekday::Sun) {
        date = date.pred_opt()?;
    }
    Some(date)
}

/// The billing period rule the import follows: an invoice covers the calendar
/// month before its issue date, except when it is issued at the end of a month
/// (on its last weekday, or the weekend after it), where it covers that month.
/// The source invoices are normally issued on the last weekday; a delayed one
/// issued in the first days of the next month still belongs to the month
/// before. Both cases derive from the one date the document prints.
fn period_suggestions(suggestions: &[PdfSuggestion]) -> Vec<PdfSuggestion> {
    let Some(issue) = suggestions
        .iter()
        .find(|suggestion| suggestion.field == PdfField::InvoiceDate)
    else {
        return Vec::new();
    };
    let Ok(issue_date) = NaiveDate::parse_from_str(&issue.value, "%Y-%m-%d") else {
        return Vec::new();
    };
    let Some((issue_start, issue_end)) = month_bounds(issue_date.year(), issue_date.month()) else {
        return Vec::new();
    };
    let Some(last_weekday) = last_weekday(issue_date.year(), issue_date.month()) else {
        return Vec::new();
    };
    let (period, reason) = if issue_date >= last_weekday {
        (
            (issue_start, issue_end),
            "Issue date is at the end of its month",
        )
    } else {
        let Some(previous_end) = issue_start.pred_opt() else {
            return Vec::new();
        };
        let Some(bounds) = month_bounds(previous_end.year(), previous_end.month()) else {
            return Vec::new();
        };
        (bounds, "Billing month preceding the issue date")
    };
    let suggestion = |field, date: NaiveDate| PdfSuggestion {
        field,
        value: date.format("%Y-%m-%d").to_string(),
        amount_minor: None,
        page: issue.page,
        evidence: reason.to_string(),
        confidence: PdfConfidence::Medium,
    };
    vec![
        suggestion(PdfField::PeriodFrom, period.0),
        suggestion(PdfField::PeriodTo, period.1),
    ]
}

fn scan_page(page: u32, text: &str, suggestions: &mut Vec<PdfSuggestion>) {
    // Harvest prints a label on its own extracted line and the value on the
    // next one (`Invoice for` / `Sedona Digital Ltd`), while other exporters
    // print `Label: value`. Both shapes are evidence; nothing is inferred from
    // position beyond the immediately following non-empty line.
    let mut pending: Option<(PdfField, &str)> = None;
    for line in text.lines().take(MAX_LINES_PER_PAGE) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some((raw_label, raw_value)) = line.split_once(':') {
            let label = normalise_label(raw_label);
            if let Some(field) = field_for_label(&label) {
                suggest(page, field, raw_value, line, suggestions);
            }
            pending = None;
            continue;
        }
        let label = normalise_label(trimmed);
        if let Some(field) = standalone_label(&label) {
            pending = Some((field, line));
            continue;
        }
        if let Some((field, evidence)) = pending.take() {
            suggest(
                page,
                field,
                trimmed,
                &format!("{evidence} {trimmed}"),
                suggestions,
            );
        }
    }
}

/// The settlement rule the import follows: payments equal to the printed total
/// mean the invoice is `paid`; anything else (including no payments line) means
/// `issued`. This is derived from the two documented amounts, never guessed.
fn status_suggestion(suggestions: &[PdfSuggestion]) -> Option<PdfSuggestion> {
    let total = suggestions
        .iter()
        .find(|suggestion| suggestion.field == PdfField::Total)
        .or_else(|| {
            suggestions
                .iter()
                .find(|suggestion| suggestion.field == PdfField::Subtotal)
        })?;
    let total_minor = total.amount_minor?;
    if total_minor < 0 {
        return None;
    }
    let paid = suggestions
        .iter()
        .find(|suggestion| suggestion.field == PdfField::Payments)
        .and_then(|suggestion| suggestion.amount_minor);
    let value = if paid == Some(total_minor) {
        "paid"
    } else {
        "issued"
    };
    Some(PdfSuggestion {
        field: PdfField::Status,
        value: value.to_string(),
        amount_minor: None,
        page: total.page,
        evidence: "Payments compared with Subtotal".to_string(),
        confidence: PdfConfidence::Medium,
    })
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
    if !failed {
        suggestions.extend(period_suggestions(&suggestions));
        if let Some(derived) = status_suggestion(&suggestions) {
            suggestions.push(derived);
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
        assert_eq!(find(PdfField::Subtotal).amount_minor, Some(100_000));
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
            "Invoice Date: 31/13/2025",
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
    fn suggests_the_harvest_layout_with_payments_and_settlement() {
        // A label on its own extracted line, the value on the next one. This is
        // the shape the real exporter produces, in contrast to `Label: value`.
        let pdf = pdf_document(&[&[
            "INVOICE",
            "Invoice for",
            "Sedona Digital Ltd",
            "33 Cavendish Square,",
            "Invoice ID",
            "1",
            "Issue date",
            "01/02/2025",
            "Subtotal",
            "EUR 5,760.00",
            "Payments",
            "EUR -5,760.00",
            "Amount due",
            "EUR 0.00",
        ]]);
        let inspection = inspect(&pdf);
        assert_eq!(
            inspection.status,
            PdfInspectionStatus::Suggestions,
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
        assert_eq!(find(PdfField::InvoiceNumber).value, "1");
        assert_eq!(find(PdfField::InvoiceDate).value, "2025-02-01");
        assert_eq!(find(PdfField::Customer).value, "Sedona Digital Ltd");
        assert_eq!(find(PdfField::Currency).value, "EUR");
        assert_eq!(find(PdfField::Subtotal).amount_minor, Some(576_000));
        assert_eq!(find(PdfField::Payments).amount_minor, Some(576_000));
        assert_eq!(find(PdfField::Status).value, "paid");
        assert_eq!(find(PdfField::PeriodFrom).value, "2025-01-01");
        assert_eq!(find(PdfField::PeriodTo).value, "2025-01-31");
    }

    #[test]
    fn an_invoice_issued_on_the_last_day_covers_its_own_month() {
        let pdf = pdf_document(&[&[
            "Issue date",
            "30/06/2025",
            "Subtotal",
            "EUR 1,000.00",
            "Payments",
            "EUR -1,000.00",
        ]]);
        let inspection = inspect(&pdf);
        let find = |field| {
            inspection
                .suggestions
                .iter()
                .find(|suggestion| suggestion.field == field)
                .unwrap_or_else(|| panic!("missing {field:?}: {:?}", inspection.suggestions))
        };
        assert_eq!(find(PdfField::InvoiceDate).value, "2025-06-30");
        assert_eq!(find(PdfField::PeriodFrom).value, "2025-06-01");
        assert_eq!(find(PdfField::PeriodTo).value, "2025-06-30");
    }

    #[test]
    fn an_invoice_issued_on_the_last_weekday_covers_its_own_month() {
        // 30 Jan 2026 is a Friday and the last weekday of January: the
        // calendar month still runs to the 31st.
        let pdf = pdf_document(&[&["Issue date", "30/01/2026"]]);
        let inspection = inspect(&pdf);
        let find = |field| {
            inspection
                .suggestions
                .iter()
                .find(|suggestion| suggestion.field == field)
                .unwrap_or_else(|| panic!("missing {field:?}: {:?}", inspection.suggestions))
        };
        assert_eq!(find(PdfField::PeriodFrom).value, "2026-01-01");
        assert_eq!(find(PdfField::PeriodTo).value, "2026-01-31");
    }

    #[test]
    fn a_delayed_january_invoice_issued_in_early_february_covers_january() {
        // 6 Feb 2026 is in the first days of the month, so the invoice bills the
        // month before it.
        let pdf = pdf_document(&[&["Issue date", "06/02/2026"]]);
        let inspection = inspect(&pdf);
        let find = |field| {
            inspection
                .suggestions
                .iter()
                .find(|suggestion| suggestion.field == field)
                .unwrap_or_else(|| panic!("missing {field:?}: {:?}", inspection.suggestions))
        };
        assert_eq!(find(PdfField::PeriodFrom).value, "2026-01-01");
        assert_eq!(find(PdfField::PeriodTo).value, "2026-01-31");
    }

    #[test]
    fn payments_below_the_printed_total_are_suggested_as_issued() {
        let pdf = pdf_document(&[&[
            "Subtotal",
            "USD $1,430.00",
            "Payments",
            "USD -$1,000.00",
            "Amount due",
            "USD $430.00",
        ]]);
        let inspection = inspect(&pdf);
        let find = |field| {
            inspection
                .suggestions
                .iter()
                .find(|suggestion| suggestion.field == field)
                .unwrap_or_else(|| panic!("missing {field:?}: {:?}", inspection.suggestions))
        };
        assert_eq!(find(PdfField::Currency).value, "USD");
        assert_eq!(find(PdfField::Subtotal).amount_minor, Some(143_000));
        assert_eq!(find(PdfField::Payments).amount_minor, Some(100_000));
        assert_eq!(find(PdfField::Status).value, "issued");
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
