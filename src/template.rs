//! Invoice document templates (#116): `%variable%` interpolation plus a
//! deliberately tiny markdown subset.
//!
//! Pure: no I/O, no clock beyond dates passed in, no randomness. The markdown
//! parses **once** into the `pdf::Block` model, consumed by `render_invoice_pdf`
//! (the layout) and `to_html` (the email; all input escaped first, so the
//! server-side rule is no weaker than the GUI's "textContent, never innerHTML").
//! Tables, links, images and raw HTML are out of the subset on purpose. Variable
//! resolution is **fail-loud at save time** (`validate_*` rejects unknown
//! `%tokens%` with the 422 `FieldError` shape) and **fail-safe at render time**
//! (an unknown token is left literal and logged — never a panic).

use std::collections::BTreeMap;

use chrono::Datelike;

use crate::domain::{Currency, FieldError, Invoice};
use crate::pdf::{Block, Doc, Span};

/// The closed set of `%variable%` names. Case-sensitive by contract.
pub const VARS: &[&str] = &[
    "invoice_number",
    "invoice_issue_day",
    "invoice_issue_month",
    "invoice_issue_month_number",
    "invoice_issue_year",
    "invoice_due_date",
    "period_from",
    "period_to",
    "period_month",
    "period_month_number",
    "period_month_padded",
    "period_year",
    "customer_name",
    "currency",
    "total",
    "line_count",
    "payment_details",
];

/// Cheatsheet rows (name, example) — the GUI renders this table, so the
/// documentation and the validation share one source of truth.
pub const VAR_HELP: &[(&str, &str)] = &[
    ("%invoice_number%", "INV-0007"),
    ("%invoice_issue_day%", "5"),
    ("%invoice_issue_month%", "October"),
    ("%invoice_issue_month_number%", "10"),
    ("%invoice_issue_year%", "2026"),
    ("%invoice_due_date%", "2026-11-04"),
    ("%period_from%", "2026-10-01"),
    ("%period_to%", "2026-10-31"),
    ("%period_month%", "October"),
    ("%period_month_number%", "10"),
    ("%period_month_padded%", "10"),
    ("%period_year%", "2026"),
    ("%customer_name%", "Tucano Time"),
    ("%currency%", "EUR"),
    ("%total%", "1,200.00"),
    ("%line_count%", "14"),
    ("%payment_details%", "IBAN: EX00 0000 ..."),
];

/// Caps (validated at save; #50 bounds on free text).
pub const SUBJECT_MAX: usize = 120;
pub const BODY_MAX: usize = 8000;
pub const FOOTER_MAX: usize = 8000;
pub const NOTES_MAX: usize = 4000;

const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

fn is_token_char(c: char) -> bool {
    // Upper case is a *candidate* too: tokens are case-sensitive, so
    // `%Total%` reads as an unknown variable (rejected at save, left literal
    // at render) instead of vanishing like stray prose percents.
    c.is_ascii_alphanumeric() || c == '_'
}

/// Scan `%token%` runs. A `%...%` is a *token candidate* only when its body
/// is non-empty and made solely of `[A-Za-z0-9_]` — stray percent signs in
/// ordinary prose (e.g. "100% paid") never form a token.
/// At `chars[i]`, recognise a `%token%`: returns the token and the number of
/// chars consumed, or `None` when this is not a token candidate.
fn token_at(chars: &[char], i: usize) -> Option<(String, usize)> {
    if chars.get(i) != Some(&'%') {
        return None;
    }
    let end = chars[i + 1..].iter().position(|&c| c == '%')?;
    let body: String = chars[i + 1..i + 1 + end].iter().collect();
    if body.is_empty() || !body.chars().all(is_token_char) {
        return None;
    }
    Some((body, end + 2))
}

fn candidates(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if let Some((token, len)) = token_at(&chars, i) {
            out.push(token);
            i += len;
        } else {
            i += 1;
        }
    }
    out
}

/// Tokens used in `text` that are not in the closed set — the save-time
/// rejection list.
pub fn unknown_tokens(text: &str) -> Vec<String> {
    candidates(text)
        .into_iter()
        .filter(|t| !VARS.contains(&t.as_str()))
        .collect()
}

/// Variables whose resolved value is a multi-line block. A subject that
/// interpolates one of these would freeze a line break into a header-bound
/// subject at issue, so the save-time validators refuse them there.
pub const MULTILINE_VARS: &[&str] = &["payment_details"];

/// Multi-line variables referenced by `text` — the save-time subject guard.
pub fn multiline_tokens(text: &str) -> Vec<String> {
    candidates(text)
        .into_iter()
        .filter(|t| MULTILINE_VARS.contains(&t.as_str()))
        .collect()
}

fn token_error(field: &str, token: &str) -> FieldError {
    FieldError::new(
        field,
        format!("unknown variable %{token}% (see the documented %variable% list)"),
    )
}

/// Validate one free-text template field: size cap + closed variable set.
/// Appends `FieldError`s to `errors`; renders nothing.
pub fn validate_field(field: &str, text: &str, max: usize, errors: &mut Vec<FieldError>) {
    if text.chars().count() > max {
        errors.push(FieldError::new(
            field,
            format!("too long (max {max} characters)"),
        ));
    }
    for token in unknown_tokens(text) {
        errors.push(token_error(field, &token));
    }
}

/// The resolved values every `%variable%` reads from. Built by `vars_for`.
pub type Vars = BTreeMap<String, String>;

/// Resolve the variable table for one (invoice, customer) pair. Dates are
/// passed by the caller (the clock lives in the API layer, not here).
pub fn vars_for(
    invoice: &Invoice,
    customer: &crate::domain::Customer,
    issue_day: Option<chrono::NaiveDate>,
) -> Vars {
    let mut v = Vars::new();
    v.insert("invoice_number".into(), invoice.number.clone());
    if let Some(d) = issue_day {
        v.insert("invoice_issue_day".into(), d.day().to_string());
        v.insert(
            "invoice_issue_month".into(),
            MONTHS[d.month0() as usize].into(),
        );
        v.insert("invoice_issue_month_number".into(), d.month().to_string());
        v.insert("invoice_issue_year".into(), d.year().to_string());
    }
    v.insert(
        "invoice_due_date".into(),
        invoice.due_date.map(|d| d.to_string()).unwrap_or_default(),
    );
    v.insert("period_from".into(), invoice.period_from.to_string());
    v.insert("period_to".into(), invoice.period_to.to_string());
    // Billing-period family (#subject): the document says "the work of
    // month/year", not "the day it was issued". Pure integer month arithmetic
    // from `period_from`; no locale, no floats.
    let period_month = invoice.period_from.month();
    v.insert(
        "period_month".into(),
        MONTHS[invoice.period_from.month0() as usize].to_string(),
    );
    v.insert("period_month_number".into(), period_month.to_string());
    v.insert("period_month_padded".into(), format!("{period_month:02}"));
    v.insert("period_year".into(), invoice.period_from.year().to_string());
    v.insert("customer_name".into(), customer.name.clone());
    v.insert("currency".into(), invoice.currency.0.clone());
    v.insert(
        "total".into(),
        crate::api::money_for_email(invoice.total_minor, &invoice.currency.0),
    );
    v.insert("line_count".into(), invoice.lines.len().to_string());
    // The customer's configured payment details ("How to pay"), empty when
    // not set — templates that reference the token simply print nothing.
    v.insert(
        "payment_details".into(),
        customer
            .payment_details
            .as_ref()
            .and_then(|p| p.email_block())
            .unwrap_or_default(),
    );
    v
}

/// Replace known `%tokens%` with their values. Unknown token candidates are
/// left **literal** with a warning (fail-safe: never panics, never guesses).
///
/// A value that spans lines is a **block**, not a wrapped phrase: each of its
/// newlines becomes a paragraph break, so the markdown parser and the HTML
/// renderer keep one line per line (`%payment_details%` is the common case).
/// Special-casing the variable name here would let a customer's own multi-line
/// value collapse instead.
#[must_use]
pub fn interpolate(text: &str, vars: &Vars) -> String {
    let mut out = String::with_capacity(text.len());
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if let Some((token, len)) = token_at(&chars, i) {
            match vars.get(&token) {
                Some(value) => {
                    if value.contains('\n') {
                        // The block must start on its own paragraph even when
                        // the token follows a source line break ("Pay to:\n
                        // %payment_details%"): one trailing newline becomes a
                        // paragraph break, an existing blank line stays as is.
                        let trailing = out.chars().rev().take_while(|c| *c == '\n').count();
                        if trailing == 1 {
                            out.push('\n');
                        }
                    }
                    for (n, line) in value.split('\n').enumerate() {
                        if n > 0 {
                            // One paragraph per newline: the parser flushes on
                            // a blank line, so the value's lines never merge.
                            out.push_str("\n\n");
                        }
                        out.push_str(line);
                    }
                    i += len;
                    continue;
                }
                None => {
                    tracing::warn!(token = %token, "template met an unknown variable, leaving it literal");
                }
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

// ---------------------------------------------------------- markdown -----

/// Parse the markdown subset into blocks. Line structure first, inline
/// styling after. Unterminated `**`/`*` markers degrade to literal text.
#[must_use]
pub fn parse_markdown(src: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut para: Vec<String> = Vec::new();
    let flush = |para: &mut Vec<String>, blocks: &mut Vec<Block>| {
        if !para.is_empty() {
            blocks.push(Block::Paragraph(spans(&para.join("\n"))));
            para.clear();
        }
    };
    for line in src.lines() {
        let t = line.trim_end();
        if t.trim().is_empty() {
            flush(&mut para, &mut blocks);
            continue;
        }
        if let Some(rest) = strip_heading(t) {
            flush(&mut para, &mut blocks);
            let level = t.chars().take_while(|c| *c == '#').count() as u8;
            blocks.push(Block::Heading {
                level,
                spans: spans(rest),
            });
        } else if let Some(rest) = t.strip_prefix("- ").or_else(|| t.strip_prefix("* "))
            && !rest.trim().is_empty()
        {
            // A bullet right after prose text starts a fresh list block.
            flush(&mut para, &mut blocks);
            blocks.push(Block::Bullet(spans(rest.trim())));
        } else if let Some((number, rest)) = strip_numbered(t) {
            flush(&mut para, &mut blocks);
            blocks.push(Block::Numbered {
                number,
                spans: spans(rest.trim()),
            });
        } else {
            para.push(t.to_string());
        }
    }
    flush(&mut para, &mut blocks);
    blocks
}

fn strip_heading(t: &str) -> Option<&str> {
    let hashes = t.chars().take_while(|c| *c == '#').count();
    if (1..=3).contains(&hashes) {
        let rest = &t[hashes..];
        // A heading marker must be followed by a space to count.
        return rest.strip_prefix(' ').or_else(|| rest.strip_prefix('\t'));
    }
    None
}

fn strip_numbered(t: &str) -> Option<(u32, &str)> {
    let digits: String = t.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    let rest = &t[digits.len()..];
    let n: u32 = digits.parse().ok()?;
    rest.strip_prefix(". ").map(|r| (n, r))
}

/// Inline spans: `**bold**`, `*italic*`. No nesting (out of the subset);
/// unmatched markers stay literal.
#[must_use]
pub fn spans(text: &str) -> Vec<Span> {
    let mut out: Vec<Span> = Vec::new();
    let mut plain = String::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    let chars = text;
    let push_plain = |plain: &mut String, out: &mut Vec<Span>| {
        if !plain.is_empty() {
            out.push(Span::text(std::mem::take(plain)));
        }
    };
    while i < chars.len() {
        if bytes[i..].starts_with(b"**")
            && let Some(end) = chars[i + 2..].find("**")
        {
            push_plain(&mut plain, &mut out);
            out.push(Span::bold(&chars[i + 2..i + 2 + end]));
            i += end + 4;
            continue;
        }
        if bytes[i] == b'*'
            && let Some(end) = chars[i + 1..].find('*')
        {
            let inner = &chars[i + 1..i + 1 + end];
            if !inner.is_empty() && !inner.contains('*') {
                push_plain(&mut plain, &mut out);
                out.push(Span::italic(inner));
                i += end + 2;
                continue;
            }
        }
        // Copy one char (byte-wise is wrong for multibyte; find the next boundary).
        let ch = chars[i..].chars().next().unwrap();
        plain.push(ch);
        i += ch.len_utf8();
    }
    push_plain(&mut plain, &mut out);
    if out.is_empty() {
        out.push(Span::text(String::new()));
    }
    out
}

// ------------------------------------------------------------- html ------

fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

fn spans_html(spans: &[Span]) -> String {
    let mut out = String::new();
    for s in spans {
        let esc = escape(&s.text).replace('\n', "<br>");
        match (s.bold, s.italic) {
            (true, true) => out.push_str(&format!("<strong><em>{esc}</em></strong>")),
            (true, false) => out.push_str(&format!("<strong>{esc}</strong>")),
            (false, true) => out.push_str(&format!("<em>{esc}</em>")),
            (false, false) => out.push_str(&esc),
        }
    }
    out
}

/// Render blocks to the HTML email part. Input was escaped before wrapping,
/// so nothing a customer typed can inject markup.
#[must_use]
pub fn to_html(blocks: &[Block]) -> String {
    let mut out = String::new();
    let mut i = 0;
    while i < blocks.len() {
        match &blocks[i] {
            Block::Heading { level, spans } => {
                let l = (*level).clamp(1, 3);
                out.push_str(&format!("<h{l}>{}</h{l}>", spans_html(spans)));
                i += 1;
            }
            Block::Paragraph(spans) => {
                out.push_str(&format!("<p>{}</p>", spans_html(spans)));
                i += 1;
            }
            Block::KeyVal { key, spans } => {
                out.push_str(&format!(
                    "<p><strong>{}:</strong> {}</p>",
                    escape(key),
                    spans_html(spans)
                ));
                i += 1;
            }
            // A separator block (payment-rail divider); the HTML letter keeps
            // it as a plain rule, never as markup a customer could inject.
            Block::Rule { .. } => {
                out.push_str("<hr>");
                i += 1;
            }
            Block::Bullet(_) => {
                out.push_str("<ul>");
                while let Some(Block::Bullet(s)) = blocks.get(i) {
                    out.push_str(&format!("<li>{}</li>", spans_html(s)));
                    i += 1;
                }
                out.push_str("</ul>");
            }
            Block::Numbered { .. } => {
                out.push_str("<ol>");
                while let Some(Block::Numbered { number: _, spans }) = blocks.get(i) {
                    out.push_str(&format!("<li>{}</li>", spans_html(spans)));
                    i += 1;
                }
                out.push_str("</ol>");
            }
        }
    }
    out
}

// ------------------------------------------------------- document glue ---

/// The heading of the document's free-text section. It is deliberately not a
/// `LabelOverrides` key: it names the *notes* section, while
/// `payment_heading` renames the generated payment block below it.
pub const NOTES_HEADING: &str = "Notes";

/// Compose the content parts of the #116 document — org body, org footer,
/// notes section, subject — from the template and customer, interpolated
/// against the invoice. Each piece is `""`/unset when absent, and empty pieces
/// produce empty vecs, keeping the caller's legacy behaviour untouched.
///
/// The notes section exists **only** when there is text: the template's
/// `notes` and/or the customer's `invoice_notes`, printed together under one
/// `Notes` heading (template text first).
#[must_use]
pub fn doc_content(
    invoice: &Invoice,
    template: &crate::domain::InvoiceTemplate,
    customer: &crate::domain::Customer,
    vars: &Vars,
) -> DocContent {
    let md = |src: &str| parse_markdown(&interpolate(src, vars));
    let mut notes = Vec::new();
    let override_notes = invoice.document.notes.as_deref();
    if override_notes.is_some_and(|text| !text.trim().is_empty())
        || (override_notes.is_none()
            && (!template.notes.trim().is_empty() || !customer.invoice_notes.trim().is_empty()))
    {
        notes.push(Block::Heading {
            level: 2,
            // Plain span: the layout bolds headings itself, and the email
            // HTML must read `<h2>Notes</h2>`, not a nested `<strong>`.
            spans: vec![Span::text(NOTES_HEADING)],
        });
        if let Some(text) = override_notes {
            notes.extend(md(text));
        } else {
            notes.extend(md(&template.notes));
            notes.extend(md(&customer.invoice_notes));
        }
    }
    DocContent {
        body: md(&template.body),
        footer: md(&template.footer),
        notes,
        subject: resolve_subject(invoice, template, customer, vars),
    }
}

/// Resolve the subject with a documented, stable precedence:
///
/// 1. **`invoice.subject`** — the snapshot frozen at issue. It is already
///    interpolated, so it wins verbatim and a later template/customer edit can
///    never rewrite an issued invoice.
/// 2. **`customer.invoice_subject`** — the live per-customer override (drafts
///    keep following edits).
/// 3. **`template.subject`** — the live org template.
/// 4. **`None`** — the caller keeps its legacy `Invoice {n} from {org}`.
fn resolve_subject(
    invoice: &Invoice,
    template: &crate::domain::InvoiceTemplate,
    customer: &crate::domain::Customer,
    vars: &Vars,
) -> Option<String> {
    let snapshot = invoice.subject.trim();
    if !snapshot.is_empty() {
        return Some(snapshot.to_string());
    }
    if let Some(subject) = &invoice.document.subject {
        return Some(interpolate(subject, vars));
    }
    let customer_subj = customer.invoice_subject.trim();
    if !customer_subj.is_empty() {
        return Some(interpolate(customer_subj, vars));
    }
    let org_subj = template.subject.trim();
    if !org_subj.is_empty() {
        return Some(interpolate(org_subj, vars));
    }
    None
}

pub struct DocContent {
    pub body: Vec<Block>,
    pub footer: Vec<Block>,
    pub notes: Vec<Block>,
    pub subject: Option<String>,
}

/// Merge template content into a `Doc` built by `pdf::doc_for`: body blocks
/// flow before the line table; the footer becomes org footer → notes section
/// → the generated payment block → the credit line. With no template and no
/// notes the doc is unchanged, which is what keeps unset templates
/// byte-identical.
pub fn apply_doc_content(doc: &mut Doc, content: &DocContent) {
    if !content.body.is_empty() {
        let mut blocks = content.body.clone();
        blocks.append(&mut doc.blocks);
        doc.blocks = blocks;
    }
    // #256: `doc.footer` is replaced wholesale below, so drain it by move
    // rather than cloning it first. `doc.footer` already ends with the
    // generated credit line, so appending it after the notes keeps that line
    // last while the customer's notes move *before* the payment block.
    let mut footer = content.footer.clone();
    footer.extend_from_slice(&content.notes);
    footer.append(&mut doc.footer);
    doc.footer = footer;
}

/// The email HTML letter: body, then footer, then notes. `None` when there
/// is no configured content (legacy: text-only emails).
#[must_use]
pub fn email_html(content: &DocContent) -> Option<String> {
    if content.body.is_empty() && content.footer.is_empty() && content.notes.is_empty() {
        return None;
    }
    let mut blocks = Vec::new();
    blocks.extend(content.body.iter().cloned());
    blocks.extend(content.footer.iter().cloned());
    blocks.extend(content.notes.iter().cloned());
    Some(to_html(&blocks))
}

/// Format helper so callers never rebuild this pair.
pub fn currency_minor(minor: u64, currency: &Currency) -> String {
    crate::api::money_for_email(minor, &currency.0)
}

// =========================================================== unit tests ===

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn sample_invoice() -> Invoice {
        use crate::domain::{Currency, InvoiceStatus};
        Invoice {
            supersedes: None,
            superseded_by: None,
            imported: None,
            id: uuid::Uuid::new_v4(),
            number: "INV-0007".into(),
            customer_id: uuid::Uuid::new_v4(),
            currency: Currency("EUR".into()),
            period_from: NaiveDate::from_ymd_opt(2026, 10, 1).unwrap(),
            period_to: NaiveDate::from_ymd_opt(2026, 10, 31).unwrap(),
            lines: vec![],
            total_minor: 120000,
            status: InvoiceStatus::Draft,
            created_at: NaiveDate::from_ymd_opt(2026, 10, 5)
                .unwrap()
                .and_hms_opt(9, 0, 0)
                .unwrap()
                .and_utc(),
            issued_at: None,
            due_date: Some(NaiveDate::from_ymd_opt(2026, 11, 4).unwrap()),
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

    fn vars() -> Vars {
        vars_for(
            &sample_invoice(),
            &customer_named("Tucano Time"),
            Some(NaiveDate::from_ymd_opt(2026, 10, 5).unwrap()),
        )
    }

    /// A customer with only a name — every other field default-ish. Built by
    /// hand because `Customer` deliberately has no `Default` impl.
    fn customer_named(name: &str) -> crate::domain::Customer {
        crate::domain::Customer {
            id: uuid::Uuid::nil(),
            name: name.into(),
            currency: crate::domain::Currency("EUR".into()),
            default_rate_minor: 1,
            active: true,
            email: String::new(),
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

    #[test]
    fn variable_table_resolves_documented_examples() {
        let issue = Some(NaiveDate::from_ymd_opt(2026, 10, 5).unwrap());
        let inv = sample_invoice();
        let built = vars_for(&inv, &customer_named("Tucano Time"), issue);
        assert_eq!(built["invoice_number"], "INV-0007");
        assert_eq!(built["invoice_issue_day"], "5");
        assert_eq!(built["invoice_issue_month"], "October");
        assert_eq!(built["invoice_issue_month_number"], "10");
        assert_eq!(built["invoice_issue_year"], "2026");
        assert_eq!(built["invoice_due_date"], "2026-11-04");
        assert_eq!(built["period_from"], "2026-10-01");
        assert_eq!(built["period_to"], "2026-10-31");
        // Billing-period family: derived from period_from, not the issue day.
        assert_eq!(built["period_month"], "October");
        assert_eq!(built["period_month_number"], "10");
        assert_eq!(built["period_month_padded"], "10");
        assert_eq!(built["period_year"], "2026");
        assert_eq!(built["customer_name"], "Tucano Time");
        assert_eq!(built["currency"], "EUR");
        assert_eq!(built["total"], "1,200.00 EUR");
        assert_eq!(built["line_count"], "0");
    }

    #[test]
    fn interpolation_replaces_known_tokens() {
        let v = vars();
        let s = interpolate(
            "Invoice %invoice_number% for %customer_name%: %total% due %invoice_due_date%",
            &v,
        );
        assert_eq!(
            s,
            "Invoice INV-0007 for Tucano Time: 1,200.00 EUR due 2026-11-04"
        );
    }

    #[test]
    fn unknown_tokens_left_literal_at_render_and_flagged_at_save() {
        let v = vars();
        assert_eq!(interpolate("Pay %bogus% now", &v), "Pay %bogus% now");
        assert_eq!(unknown_tokens("Pay %bogus% and %total%"), vec!["bogus"]);
        let mut errors = Vec::new();
        validate_field("body", "Pay %bogus%", BODY_MAX, &mut errors);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].field, "body");
        // Case-sensitive: %Total% is not a token candidate spelling we know.
        assert_eq!(unknown_tokens("%Total%"), vec!["Total"]);
    }

    #[test]
    fn stray_percent_never_forms_a_token() {
        assert!(candidates("100% paid, 50% deposit").is_empty());
        assert_eq!(interpolate("100% done", &vars()), "100% done");
    }

    #[test]
    fn markdown_subset_parses_blocks() {
        let md = "# Title\nIntro text\n\n- one\n- two\n\n1. first\n2. second\n\n### Small **bold** and *it*";
        let blocks = parse_markdown(md);
        assert!(matches!(&blocks[0], Block::Heading { level: 1, .. }));
        assert!(matches!(&blocks[1], Block::Paragraph(_)));
        assert!(matches!(&blocks[2], Block::Bullet(_)));
        assert!(matches!(&blocks[3], Block::Bullet(_)));
        assert!(matches!(&blocks[4], Block::Numbered { number: 1, .. }));
        assert!(matches!(&blocks[5], Block::Numbered { number: 2, .. }));
        match &blocks[6] {
            Block::Heading { level: 3, spans } => {
                assert_eq!(spans[0].text, "Small ");
                assert!(spans[1].bold);
                assert_eq!(spans[2].text, " and ");
                assert!(spans[3].italic);
            }
            other => panic!("expected heading, got {other:?}"),
        }
    }

    #[test]
    fn markdown_escapes_html_and_blocks_unsupported_syntax() {
        let html = to_html(&parse_markdown(
            "<script>alert(1)</script> and |a|b| tables [x](y)",
        ));
        assert!(!html.contains("<script>"), "{html}");
        assert!(html.contains("&lt;script&gt;"), "{html}");
        // Tables/links/images are literal text, never markup.
        assert!(html.contains("[x](y)"), "{html}");
    }

    #[test]
    fn unmatched_style_markers_stay_literal() {
        let s = interpolate("50% off **today", &Vars::new());
        assert_eq!(s, "50% off **today");
        let blocks = parse_markdown("a ** b");
        match &blocks[0] {
            Block::Paragraph(spans) => assert!(spans.iter().all(|sp| !sp.bold)),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn html_wraps_lists_and_breaks_lines() {
        let html = to_html(&parse_markdown("- a\n- b"));
        assert!(html.starts_with("<ul><li>a</li><li>b</li></ul>"), "{html}");
        let p = to_html(&parse_markdown("line one\nline two"));
        assert!(p.contains("line one<br>line two"), "{p}");
    }

    #[test]
    fn subject_precedence_customer_then_org_then_none() {
        use crate::domain::{Currency, Customer, InvoiceTemplate};
        let customer = Customer {
            id: uuid::Uuid::new_v4(),
            name: "C".into(),
            currency: Currency("EUR".into()),
            default_rate_minor: 1,
            active: true,
            email: String::new(),
            payment_terms: None,
            invoice_notes: String::new(),
            invoice_subject: "%invoice_number% for %customer_name%".into(),
            invoice_prefix: String::new(),
            address: None,
            contacts: vec![],
            tax_hundredths: 0,
            discount_hundredths: 0,

            payment_details: None,
            legal_id: String::new(),
        };
        let template = InvoiceTemplate {
            subject: "Invoice %invoice_number%".into(),
            ..Default::default()
        };
        let v = vars();
        let draft = sample_invoice();
        assert_eq!(
            resolve_subject(&draft, &template, &customer, &v).as_deref(),
            Some("INV-0007 for Tucano Time")
        );
        let bare = Customer {
            invoice_subject: String::new(),
            invoice_prefix: Default::default(),
            ..customer.clone()
        };
        assert_eq!(
            resolve_subject(&draft, &template, &bare, &v).as_deref(),
            Some("Invoice INV-0007")
        );
        let empty = crate::domain::InvoiceTemplate::default();
        assert_eq!(resolve_subject(&draft, &empty, &bare, &v), None);
        // The issue-time snapshot wins verbatim over both live sources.
        let issued = Invoice {
            subject: "Tucano Time invoice for 08 2026".into(),
            ..sample_invoice()
        };
        assert_eq!(
            resolve_subject(&issued, &template, &customer, &v).as_deref(),
            Some("Tucano Time invoice for 08 2026")
        );
    }

    #[test]
    fn period_variables_use_integer_month_arithmetic() {
        let inv = Invoice {
            period_from: NaiveDate::from_ymd_opt(2026, 1, 15).unwrap(),
            period_to: NaiveDate::from_ymd_opt(2026, 1, 31).unwrap(),
            ..sample_invoice()
        };
        let v = vars_for(&inv, &customer_named("C"), None);
        assert_eq!(v["period_month"], "January");
        assert_eq!(v["period_month_number"], "1");
        assert_eq!(v["period_month_padded"], "01");
        assert_eq!(v["period_year"], "2026");
        // The default seeded subject expands to the documented wording.
        let template = crate::domain::InvoiceTemplate::seeded();
        let rendered = interpolate(&template.subject, &v);
        assert_eq!(rendered, "C invoice for 01 2026");
    }

    #[test]
    fn unset_template_leaves_the_doc_untouched() {
        use crate::domain::{Currency, Customer, InvoiceTemplate};
        let customer = Customer {
            id: uuid::Uuid::new_v4(),
            name: "C".into(),
            currency: Currency("EUR".into()),
            default_rate_minor: 1,
            active: true,
            email: String::new(),
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
        };
        let template = InvoiceTemplate::default();
        let inv = sample_invoice();
        let mut doc = crate::pdf::doc_for(&inv, &customer, &crate::pdf::Org::named("Tucano"));
        let before = doc.clone();
        let content = doc_content(&inv, &template, &customer, &vars_for(&inv, &customer, None));
        apply_doc_content(&mut doc, &content);
        assert_eq!(doc.blocks, before.blocks);
        assert_eq!(doc.footer, before.footer);
        assert!(email_html(&content).is_none(), "unset template: text-only");
    }

    #[test]
    fn template_content_flows_into_the_doc_and_email() {
        use crate::domain::{Currency, Customer, InvoiceTemplate};
        let customer = Customer {
            id: uuid::Uuid::new_v4(),
            name: "Tucano Time".into(),
            currency: Currency("EUR".into()),
            default_rate_minor: 1,
            active: true,
            email: String::new(),
            payment_terms: None,
            invoice_notes: "Thanks for the swift payment!".into(),
            invoice_subject: String::new(),
            invoice_prefix: Default::default(),
            address: None,
            contacts: vec![],
            tax_hundredths: 0,
            discount_hundredths: 0,

            payment_details: None,
            legal_id: String::new(),
        };
        let template = InvoiceTemplate {
            body: "## Work for %invoice_issue_month%\n- consultancy".into(),
            footer: "Bank: IBAN XX01".into(),
            ..Default::default()
        };
        let inv = sample_invoice();
        let mut doc = crate::pdf::doc_for(&inv, &customer, &crate::pdf::Org::named("Tucano"));
        let content = doc_content(
            &inv,
            &template,
            &customer,
            &vars_for(
                &inv,
                &customer,
                Some(NaiveDate::from_ymd_opt(2026, 10, 5).unwrap()),
            ),
        );
        apply_doc_content(&mut doc, &content);
        assert!(
            matches!(&doc.blocks[0], Block::Heading { level: 2, spans } if spans[0].text == "Work for October")
        );
        // Org footer, then the `Notes` section; the generated credit line is a
        // page footer, not body content.
        let footer: Vec<String> = doc
            .footer
            .iter()
            .map(|b| match b {
                Block::Heading { spans, .. } => format!("heading:{}", spans[0].text),
                Block::Paragraph(spans) => spans[0].text.clone(),
                other => format!("{other:?}"),
            })
            .collect();
        assert_eq!(footer[0], "Bank: IBAN XX01", "{footer:?}");
        assert_eq!(footer[1], "heading:Notes", "{footer:?}");
        assert_eq!(footer[2], "Thanks for the swift payment!", "{footer:?}");
        assert!(
            doc.credit.starts_with("Generated by"),
            "the credit is a page footer now: {footer:?}"
        );
        let html = email_html(&content).unwrap();
        assert!(html.contains("<h2>Work for October</h2>"), "{html}");
        assert!(html.contains("Bank: IBAN XX01"), "{html}");
        assert!(html.contains("<h2>Notes</h2>"), "{html}");
        assert!(html.contains("swift payment"), "{html}");
    }

    /// The notes section exists only when there is text. An empty template
    /// `notes` and an empty customer note produce no heading at all.
    #[test]
    fn empty_notes_produce_no_heading() {
        use crate::domain::InvoiceTemplate;
        let inv = sample_invoice();
        let customer = customer_named("C");
        let content = doc_content(&inv, &InvoiceTemplate::default(), &customer, &vars());
        assert!(content.notes.is_empty(), "{:?}", content.notes);
    }

    /// The org-wide `notes` and the customer's `invoice_notes` share one
    /// `Notes` section: template text first, customer text after it, and the
    /// whole section prints before the generated payment block.
    #[test]
    fn notes_section_orders_template_then_customer_before_payment() {
        use crate::domain::{Customer, InvoiceTemplate, PaymentDetails, PaymentRail};
        let inv = sample_invoice();
        let customer = Customer {
            invoice_notes: "Customer note.".into(),
            payment_details: Some(PaymentDetails {
                iban: "EX000000000000000000".into(),
                rails: vec![PaymentRail {
                    currency: "EUR".into(),
                    account_name: "Holder".into(),
                    ..Default::default()
                }],
                ..Default::default()
            }),
            ..customer_named("C")
        };
        let template = InvoiceTemplate {
            footer: "Org footer.".into(),
            notes: "Org note.".into(),
            ..Default::default()
        };
        let mut doc = crate::pdf::doc_for(&inv, &customer, &crate::pdf::Org::named("Tucano"));
        let content = doc_content(&inv, &template, &customer, &vars_for(&inv, &customer, None));
        apply_doc_content(&mut doc, &content);
        let rows: Vec<String> = doc
            .footer
            .iter()
            .map(|b| match b {
                Block::Heading { spans, .. } => format!("heading:{}", spans[0].text),
                Block::Paragraph(spans) => spans[0].text.clone(),
                Block::KeyVal { key, .. } => format!("row:{key}"),
                other => format!("{other:?}"),
            })
            .collect();
        let position = |needle: &str| {
            rows.iter()
                .position(|row| row == needle)
                .unwrap_or_else(|| panic!("{needle} missing from {rows:?}"))
        };
        assert!(position("Org footer.") < position("heading:Notes"));
        assert!(position("heading:Notes") < position("Org note."));
        assert!(position("Org note.") < position("Customer note."));
        assert!(
            position("Customer note.") < position("heading:Payment details"),
            "{rows:?}"
        );
        assert!(
            doc.credit.starts_with("Generated by"),
            "the credit is a page footer: {rows:?}"
        );
        // Email letter follows the same content order.
        let html = email_html(&content).unwrap();
        let org = html.find("Org note.").unwrap();
        let cust = html.find("Customer note.").unwrap();
        assert!(org < cust, "{html}");
    }

    /// A multi-line interpolated value is a block: every newline becomes its
    /// own paragraph in the PDF blocks and in the email HTML. Uses a two-rail
    /// customer so the real `%payment_details%` value is exercised.
    #[test]
    fn multiline_variable_expands_to_one_paragraph_per_line() {
        use crate::domain::{Customer, InvoiceTemplate, PaymentDetails, PaymentRail};
        let inv = sample_invoice();
        let customer = Customer {
            payment_details: Some(PaymentDetails {
                bank_name: "Example Bank".into(),
                account_name: "Holder".into(),
                iban: "EX000000000000000000".into(),
                instructions: "Quote the invoice number.".into(),
                rails: vec![
                    PaymentRail {
                        currency: "EUR".into(),
                        account_name: "Holder".into(),
                        iban: Some("EX111111111111111111".into()),
                        ..Default::default()
                    },
                    PaymentRail {
                        currency: "USD".into(),
                        account_name: "Holder".into(),
                        account_number: Some("000000000000".into()),
                        ..Default::default()
                    },
                ],
                ..Default::default()
            }),
            ..customer_named("C")
        };
        let template = InvoiceTemplate {
            notes: "Pay to:\n%payment_details%".into(),
            ..Default::default()
        };
        let content = doc_content(&inv, &template, &customer, &vars_for(&inv, &customer, None));
        let paragraphs: Vec<String> = content
            .notes
            .iter()
            .filter_map(|b| match b {
                Block::Paragraph(spans) => Some(spans[0].text.clone()),
                _ => None,
            })
            .collect();
        // The "Pay to:" line and every payment line keep their own paragraph.
        assert!(
            paragraphs.contains(&"Pay to:".to_string()),
            "{paragraphs:?}"
        );
        assert!(
            paragraphs.contains(&"Bank: Example Bank".to_string()),
            "{paragraphs:?}"
        );
        assert!(
            paragraphs.contains(&"IBAN: EX000000000000000000".to_string()),
            "{paragraphs:?}"
        );
        assert!(
            paragraphs.contains(&"Quote the invoice number.".to_string()),
            "{paragraphs:?}"
        );
        assert!(
            paragraphs.contains(&"Currency: USD".to_string()),
            "{paragraphs:?}"
        );
        assert!(
            paragraphs.iter().all(|p| !p.contains('\n')),
            "a multi-line value never stays one collapsed span: {paragraphs:?}"
        );
        let html = email_html(&content).unwrap();
        assert!(html.contains("<p>Pay to:</p>"), "{html}");
        assert!(html.contains("<p>Bank: Example Bank</p>"), "{html}");
        assert!(html.contains("<p>Currency: USD</p>"), "{html}");
        assert!(!html.contains("Pay to:<br>"), "{html}");
    }

    /// A legacy template JSON without `notes` loads with an empty field and
    /// round-trips without inventing the key.
    #[test]
    fn legacy_template_without_notes_loads_and_round_trips() {
        use crate::domain::InvoiceTemplate;
        let legacy = r#"{"subject":"Invoice %invoice_number%","body":"b","footer":"f","payment_terms":null}"#;
        let template: InvoiceTemplate = serde_json::from_str(legacy).unwrap();
        assert_eq!(template.notes, "");
        let serialized = serde_json::to_string(&template).unwrap();
        assert!(
            !serialized.contains("notes"),
            "an empty notes field is not serialized: {serialized}"
        );
        // And the seeded template keeps the historical bytes: no notes key.
        let seeded = serde_json::to_string(&crate::domain::InvoiceTemplate::seeded()).unwrap();
        assert!(!seeded.contains("notes"), "{seeded}");
    }

    #[test]
    fn subject_guard_flags_multiline_variables() {
        assert_eq!(
            multiline_tokens("Invoice %payment_details%"),
            vec!["payment_details"]
        );
        assert!(multiline_tokens("Invoice %invoice_number%").is_empty());
        // The literal-token scanner never mistakes stray percent signs.
        assert!(multiline_tokens("100% paid").is_empty());
    }
}
