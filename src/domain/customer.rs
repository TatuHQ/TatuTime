use super::primitives::{
    Currency, FieldError, default_active, is_zero_u16, validate_name, validate_rate_minor,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Customer {
    pub id: Uuid,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub invoice_prefix: String,
    pub name: String,
    pub currency: Currency,
    pub default_rate_minor: u64,
    pub active: bool,
    /// Optional billing email for invoice delivery (#35).
    #[serde(default)]
    pub email: String,
    /// Payment terms used for `due_date` at issue (#116). `None` defers to
    /// the org template's default, then the legacy net-14-from-period-to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payment_terms: Option<PaymentTerms>,
    /// Markdown-subset notes appended to this customer's invoice documents
    /// after the org footer (#116). Cap enforced by validation.
    #[serde(default)]
    pub invoice_notes: String,
    /// Per-customer subject override; `%variable%` placeholders allowed
    /// (#116). Empty defers to the org template, then the legacy subject.
    #[serde(default)]
    pub invoice_subject: String,
    /// Postal address for invoice documents and e-invoicing (#139).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address: Option<Address>,
    /// Named contacts; the one flagged `billing` receives invoice emails
    /// when set (#139). Legacy customers have none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub contacts: Vec<Contact>,
    /// Default VAT percent in HUNDREDTHS of a percent (2100 = 21.00%).
    /// Integer, never a float; #143's line model will consume it.
    #[serde(default, skip_serializing_if = "is_zero_u16")]
    pub tax_hundredths: u16,
    /// Default discount percent, same hundredths encoding, applied by #143.
    #[serde(default, skip_serializing_if = "is_zero_u16")]
    pub discount_hundredths: u16,
    /// Company payment details configured for this customer ("Get paid"
    /// panel, invoice emails, `%payment_details%`). `None` on legacy docs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payment_details: Option<PaymentDetails>,
    /// The customer's company/registration number as printed on documents
    /// (e.g. `99999999`). Empty on legacy documents.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub legal_id: String,
}

/// A postal address; every part is free text with validated bounds (#139).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Address {
    #[serde(default)]
    pub street: String,
    #[serde(default)]
    pub city: String,
    #[serde(default)]
    pub postal_code: String,
    #[serde(default)]
    pub country: String,
}

/// A named person at a customer (#139). `billing` marks the invoice
/// recipient; at most one contact may carry it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Contact {
    pub name: String,
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub email: String,
    #[serde(default)]
    pub billing: bool,
}

/// The company's payment details **for one customer** — the answer to "how does
/// this client pay us". The GUI's *Get paid* panel, the invoice/reminder emails
/// and the `%payment_details%` document variable all render this, with no provider
/// integration; every part is validated free text and an all-empty block never persists.
///
/// ```rust
/// details.iban = "EX00 0000 0000 0000 0000 00".into();
/// // -> the "How to pay" section of the invoice, its email and its PDF
/// ```
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PaymentDetails {
    /// The bank the money goes to.
    #[serde(default)]
    pub bank_name: String,
    /// Account holder name as the bank knows it.
    #[serde(default)]
    pub account_name: String,
    /// Local account number.
    #[serde(default)]
    pub account_number: String,
    /// SEPA IBAN (preferred for transfers where IBAN is the norm).
    #[serde(default)]
    pub iban: String,
    /// BIC/SWIFT code.
    #[serde(default)]
    pub bic: String,
    /// What the payer should quote as reference (e.g. "%invoice_number%"
    /// is NOT expanded here — the ledger/due-date flow keeps invoice
    /// numbers; this is a plain hint like "Invoice number").
    #[serde(default)]
    pub reference: String,
    /// Free-form instructions shown verbatim last.
    #[serde(default)]
    pub instructions: String,
    /// Structured rails for multi-currency billing (one account per
    /// currency). The flat fields above stay authoritative for the
    /// single-rail case; nothing is migrated and no provider integration is
    /// involved — this is display data only.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rails: Vec<PaymentRail>,
}

/// At most this many structured payment rails may be configured.
pub const MAX_PAYMENT_RAILS: usize = 4;

/// One currency-specific payment rail (e.g. a EUR IBAN and a USD account in
/// the same [`PaymentDetails`] block). Display data only.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PaymentRail {
    /// ISO-4217 code, exactly 3 upper-case letters (`EUR`, `USD`).
    pub currency: String,
    /// Account holder name as the bank knows it (required on a rail).
    pub account_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_number: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iban: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bic: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bank_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bank_address: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routing_number: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub swift: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
}

impl PaymentRail {
    /// Trimmed copy for persistence (optionals stay absent when blank).
    #[must_use]
    pub fn trimmed(&self) -> Self {
        let trim = |v: &Option<String>| {
            v.as_ref()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        };
        Self {
            currency: self.currency.trim().to_uppercase(),
            account_name: self.account_name.trim().to_string(),
            account_number: trim(&self.account_number),
            account_type: trim(&self.account_type),
            iban: trim(&self.iban),
            bic: trim(&self.bic),
            bank_name: trim(&self.bank_name),
            bank_address: trim(&self.bank_address),
            routing_number: trim(&self.routing_number),
            swift: trim(&self.swift),
            instructions: trim(&self.instructions),
        }
    }

    /// Structured `(label, value)` rows for the rail, only non-empty fields.
    #[must_use]
    pub fn rows(&self) -> Vec<(&'static str, String)> {
        let mut out = vec![("Currency", self.currency.trim().to_string())];
        for (label, value) in [
            ("Bank", self.bank_name.as_deref()),
            ("Account name", Some(self.account_name.as_str())),
            ("Account number", self.account_number.as_deref()),
            ("Account type", self.account_type.as_deref()),
            ("IBAN", self.iban.as_deref()),
            ("BIC/SWIFT", self.bic.as_deref()),
            ("Bank address", self.bank_address.as_deref()),
            ("Routing number", self.routing_number.as_deref()),
            ("SWIFT", self.swift.as_deref()),
        ] {
            if let Some(v) = value.map(str::trim).filter(|v| !v.is_empty()) {
                out.push((label, v.to_string()));
            }
        }
        out
    }
}

impl PaymentDetails {
    /// True when any field carries content; an all-blank block is not
    /// worth persisting (the input normaliser maps it to `None`).
    #[must_use]
    pub fn has_content(&self) -> bool {
        [
            &self.bank_name,
            &self.account_name,
            &self.account_number,
            &self.iban,
            &self.bic,
            &self.reference,
            &self.instructions,
        ]
        .iter()
        .any(|v| !v.trim().is_empty())
            || self.rails.iter().any(|r| !r.currency.trim().is_empty())
    }

    /// Trimmed copy for persistence.
    #[must_use]
    pub fn trimmed(&self) -> Self {
        Self {
            bank_name: self.bank_name.trim().to_string(),
            account_name: self.account_name.trim().to_string(),
            account_number: self.account_number.trim().to_string(),
            iban: self.iban.trim().to_string(),
            bic: self.bic.trim().to_string(),
            reference: self.reference.trim().to_string(),
            instructions: self.instructions.trim().to_string(),
            rails: self.rails.iter().map(PaymentRail::trimmed).collect(),
        }
    }

    /// Structured `(label, value)` rows for the GUI's Get-paid panel — only
    /// non-empty fields, in display order. The caller decides layout; the
    /// GUI inserts them with `textContent`, never HTML.
    #[must_use]
    pub fn rows(&self) -> Vec<(&'static str, String)> {
        let mut out: Vec<(&'static str, String)> = Vec::new();
        for (label, value) in [
            ("Bank", &self.bank_name),
            ("Account name", &self.account_name),
            ("Account number", &self.account_number),
            ("IBAN", &self.iban),
            ("BIC/SWIFT", &self.bic),
            ("Reference", &self.reference),
        ] {
            if !value.trim().is_empty() {
                out.push((label, value.trim().to_string()));
            }
        }
        out
    }

    /// The canonical "How to pay" block every surface renders: the flat rows
    /// and flat instructions first, then each currency rail's rows and
    /// instructions, with a separator between the flat block and the rails and
    /// between rails. The PDF, the email text and `%payment_details%` all read
    /// this ONE sequence, so no surface can drift from another.
    #[must_use]
    pub fn canonical_lines(&self) -> Vec<PaymentLine> {
        let mut lines = Vec::new();
        for (label, value) in self.rows() {
            lines.push(PaymentLine::Row(label, value));
        }
        let instructions = self.instructions.trim();
        if !instructions.is_empty() {
            lines.push(PaymentLine::Text(instructions.to_string()));
        }
        for rail in &self.rails {
            if rail.currency.trim().is_empty() {
                continue;
            }
            // A rail is its own section: separate it from whatever came
            // before (the flat block or the previous rail).
            if !lines.is_empty() {
                lines.push(PaymentLine::Separator);
            }
            for (label, value) in rail.rows() {
                lines.push(PaymentLine::Row(label, value));
            }
            if let Some(instructions) = rail
                .instructions
                .as_deref()
                .map(str::trim)
                .filter(|v| !v.is_empty())
            {
                lines.push(PaymentLine::Text(instructions.to_string()));
            }
        }
        lines
    }

    /// Plain-text block for emails and the `%payment_details%` document
    /// variable, rendered from [`PaymentDetails::canonical_lines`] so it is
    /// byte-for-byte the same content the PDF prints. `None` when empty so
    /// senders can omit the section honestly.
    #[must_use]
    pub fn email_block(&self) -> Option<String> {
        let lines: Vec<String> = self
            .canonical_lines()
            .into_iter()
            .filter_map(|line| match line {
                PaymentLine::Row(label, value) => Some(format!("{label}: {value}")),
                PaymentLine::Text(text) => Some(text),
                // A separator is a visual break on paper; the text block
                // already reads as one section per currency.
                PaymentLine::Separator => None,
            })
            .collect();
        if lines.is_empty() {
            None
        } else {
            Some(lines.join("\n"))
        }
    }
}

/// One line of the canonical payment block. `Separator` marks the break
/// between the flat block and the currency rails (and between rails); the PDF
/// draws it as a dashed rule, the text/HTML renderings omit it.
#[derive(Debug, Clone, PartialEq)]
pub enum PaymentLine {
    /// A labelled row: `(label, value)`.
    Row(&'static str, String),
    /// Free-text instruction lines (newlines preserved).
    Text(String),
    /// A section break between the flat block and a rail.
    Separator,
}

impl Customer {
    /// Where invoice emails go: the flagged billing contact's address when
    /// present, else the legacy top-level email (#139 keeps both truthful).
    #[must_use]
    pub fn billing_email(&self) -> &str {
        self.contacts
            .iter()
            .find(|c| c.billing && !c.email.trim().is_empty())
            .map(|c| c.email.trim())
            .unwrap_or(self.email.trim())
    }
}

pub const MAX_CONTACTS: usize = 10;

/// Validate the #139 additions; appends FieldErrors like every other input.
pub fn validate_customer_billing_fields(
    address: &Option<Address>,
    contacts: &[Contact],
    tax_hundredths: u16,
    discount_hundredths: u16,
    payment_details: &Option<PaymentDetails>,
    errors: &mut Vec<FieldError>,
) {
    if let Some(a) = address {
        for (field, val, max) in [
            ("address.street", &a.street, 120usize),
            ("address.city", &a.city, 80),
            ("address.postal_code", &a.postal_code, 16),
            ("address.country", &a.country, 56),
        ] {
            if val.chars().count() > max {
                errors.push(FieldError::new(field, format!("too long (max {max})")));
            }
        }
        let any = [
            a.street.trim(),
            a.city.trim(),
            a.postal_code.trim(),
            a.country.trim(),
        ]
        .iter()
        .any(|v| !v.is_empty());
        if any && a.country.trim().len() < 2 {
            errors.push(FieldError::new(
                "address.country",
                "required when addressing an invoice",
            ));
        }
    }
    if contacts.len() > MAX_CONTACTS {
        errors.push(FieldError::new(
            "contacts",
            format!("at most {MAX_CONTACTS} contacts"),
        ));
    }
    for (i, c) in contacts.iter().enumerate() {
        let name = format!("contacts[{i}].name");
        if c.name.trim().is_empty() || c.name.chars().count() > 120 {
            errors.push(FieldError::new(name, "required, at most 120 characters"));
        }
        if c.role.chars().count() > 60 {
            errors.push(FieldError::new(
                format!("contacts[{i}].role"),
                "at most 60 characters",
            ));
        }
        let email = c.email.trim();
        if !email.is_empty() && !crate::email::valid_address(email) {
            errors.push(FieldError::new(
                format!("contacts[{i}].email"),
                "not a valid email address",
            ));
        }
    }
    if contacts.iter().filter(|c| c.billing).count() > 1 {
        errors.push(FieldError::new(
            "contacts",
            "only one contact can be the billing contact",
        ));
    }
    for (field, v) in [
        ("tax_hundredths", tax_hundredths),
        ("discount_hundredths", discount_hundredths),
    ] {
        if v > 10_000 {
            errors.push(FieldError::new(field, "at most 100.00 percent"));
        }
    }
    if let Some(p) = payment_details {
        for (field, val, max) in [
            ("payment_details.bank_name", &p.bank_name, 120usize),
            ("payment_details.account_name", &p.account_name, 120),
            ("payment_details.account_number", &p.account_number, 64),
            ("payment_details.iban", &p.iban, 34),
            ("payment_details.bic", &p.bic, 11),
            ("payment_details.reference", &p.reference, 60),
            ("payment_details.instructions", &p.instructions, 500),
        ] {
            if val.chars().count() > max {
                errors.push(FieldError::new(field, format!("too long (max {max})")));
            }
        }
        // Shapes the document must not print nonsense for: IBAN is up to 34
        // alphanumerics (no checksum library — no false negatives), BIC is
        // exactly 8 or 11. Empty means "not used", which is always valid.
        let iban = p.iban.trim();
        if !iban.is_empty() && (iban.len() < 5 || !iban.chars().all(|c| c.is_ascii_alphanumeric()))
        {
            errors.push(FieldError::new(
                "payment_details.iban",
                "must be 5-34 letters/digits",
            ));
        }
        let bic = p.bic.trim();
        if !bic.is_empty()
            && ((bic.len() != 8 && bic.len() != 11)
                || !bic.chars().all(|c| c.is_ascii_alphanumeric()))
        {
            errors.push(FieldError::new(
                "payment_details.bic",
                "must be 8 or 11 letters/digits",
            ));
        }
        // Structured multi-currency rails: bounded count, a real ISO code and
        // the same shape rules the flat single-rail block follows.
        if p.rails.len() > MAX_PAYMENT_RAILS {
            errors.push(FieldError::new(
                "payment_details.rails",
                format!("at most {MAX_PAYMENT_RAILS} payment rails"),
            ));
        }
        for (i, rail) in p.rails.iter().enumerate() {
            let field = |suffix: &str| format!("payment_details.rails[{i}].{suffix}");
            let currency = rail.currency.trim();
            if currency.len() != 3 || !currency.chars().all(|c| c.is_ascii_uppercase()) {
                errors.push(FieldError::new(
                    field("currency"),
                    "required, a 3-letter uppercase ISO code",
                ));
            }
            // `account_name` is a required rail field (its absence is not a
            // blank optional the way account_number/iban are): a rail that
            // names no account holder cannot be paid into. Refused on write,
            // with the same wording as every other required field.
            if rail.account_name.trim().is_empty() {
                errors.push(FieldError::new(
                    field("account_name"),
                    "required, at most 120 characters",
                ));
            }
            for (name, val, max) in [
                ("account_name", rail.account_name.as_str(), 120usize),
                (
                    "account_number",
                    rail.account_number.as_deref().unwrap_or(""),
                    64,
                ),
                (
                    "account_type",
                    rail.account_type.as_deref().unwrap_or(""),
                    40,
                ),
                ("iban", rail.iban.as_deref().unwrap_or(""), 34),
                ("bic", rail.bic.as_deref().unwrap_or(""), 11),
                ("bank_name", rail.bank_name.as_deref().unwrap_or(""), 120),
                (
                    "bank_address",
                    rail.bank_address.as_deref().unwrap_or(""),
                    200,
                ),
                (
                    "routing_number",
                    rail.routing_number.as_deref().unwrap_or(""),
                    40,
                ),
                ("swift", rail.swift.as_deref().unwrap_or(""), 11),
                (
                    "instructions",
                    rail.instructions.as_deref().unwrap_or(""),
                    500,
                ),
            ] {
                if val.chars().count() > max {
                    errors.push(FieldError::new(
                        field(name),
                        format!("too long (max {max})"),
                    ));
                }
            }
            let rail_iban = rail.iban.as_deref().unwrap_or("").trim();
            if !rail_iban.is_empty()
                && (rail_iban.len() < 5 || !rail_iban.chars().all(|c| c.is_ascii_alphanumeric()))
            {
                errors.push(FieldError::new(
                    field("iban"),
                    "must be 5-34 letters/digits",
                ));
            }
            for name in ["bic", "swift"] {
                let code = match name {
                    "bic" => rail.bic.as_deref().unwrap_or("").trim(),
                    _ => rail.swift.as_deref().unwrap_or("").trim(),
                };
                if !code.is_empty()
                    && ((code.len() != 8 && code.len() != 11)
                        || !code.chars().all(|c| c.is_ascii_alphanumeric()))
                {
                    errors.push(FieldError::new(
                        field(name),
                        "must be 8 or 11 letters/digits",
                    ));
                }
            }
        }
    }
}

/// Payment terms that decide an invoice's `due_date` at issue (#116).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub struct PaymentTerms {
    pub kind: TermsKind,
    /// Only meaningful for `custom`; `1..=365`. Other kinds must omit it
    /// (validation rejects it otherwise, serde defaults it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub days: Option<u16>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TermsKind {
    /// Due the day the invoice is issued.
    #[serde(rename = "upon_receipt")]
    UponReceipt,
    #[serde(rename = "net_15")]
    Net15,
    #[serde(rename = "net_20")]
    Net20,
    #[serde(rename = "net_30")]
    Net30,
    #[serde(rename = "net_45")]
    Net45,
    /// `days` required, 1..=365.
    #[serde(rename = "custom")]
    Custom,
}

impl PaymentTerms {
    /// The fixed-day kinds carry their days implicitly; `custom` and
    /// `upon_receipt` do not.
    pub fn fixed_days(&self) -> Option<u16> {
        match self.kind {
            TermsKind::UponReceipt => Some(0),
            TermsKind::Net15 => Some(15),
            TermsKind::Net20 => Some(20),
            TermsKind::Net30 => Some(30),
            TermsKind::Net45 => Some(45),
            TermsKind::Custom => self.days,
        }
    }
}

/// Organization legal identity for invoice documents and emails (#138),
/// stored as the singleton `org_profile.json`. `name` empty means "fall back
/// to the boot-time `org_name` config key" — a documented resolution order
/// (org_profile > config org_name > built-in default), never parallel truth:
/// every reader goes through `api::invoicing::org_for`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrgProfile {
    /// Display name on documents (falls back to config `org_name`).
    #[serde(default)]
    pub name: String,
    /// VAT / tax / company registration identifier printed on documents.
    #[serde(default)]
    pub legal_id: String,
    #[serde(default)]
    pub address: Option<Address>,
    /// Sender display name for invoice emails (#146); empty = built-in
    /// "TucanoTime". The SMTP password/username stay in the vault — this is
    /// presentation only.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub from_name: String,
    /// Reply-To address for invoice emails (#146); empty omits the header.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reply_to: String,
    /// Invoice document accent, '#rrggbb' (#146); empty keeps the built-in
    /// ink-only rendering. Applied to the totals rule + amount text.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub accent: String,
    /// Display prefix for [`OrgProfile::legal_id`] (`Tax ID`, `VAT`,
    /// `Company number`). Empty keeps today's `VAT / company id` wording.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub legal_id_label: String,
    /// The company accountant's address (#112 follow-up). Prefills the
    /// "send a PDF copy" dialog and is offered when an invoice is marked paid.
    /// Empty means "ask each time"; it is display/recipient data only and is
    /// never printed on a document.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub accountant_email: String,
}

/// Display-label overrides for invoice documents (#147). Empty = built-in
/// label; only DISPLAY changes — API/JSON field names are untouched.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LabelOverrides {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub quantity: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub unit_price: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub subtotal: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub discount: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub tax: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub total: String,
    /// Wording of the issuer block heading (`From`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub from: String,
    /// Wording of the customer block heading (`Invoice for`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub invoice_for: String,
    /// Wording of the invoice-number row (`Invoice ID`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub invoice_id: String,
    /// Wording of the issue-date row (`Issue date`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub issue_date: String,
    /// Wording of the due-date row (`Due date`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub due_date: String,
    /// Wording of the subject row (`Subject`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub subject: String,
    /// Wording of the payment section: labels both the structured rails
    /// (built-in `Notes`) and the legacy flat block (built-in
    /// `Payment details`) when set.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub payment_heading: String,
    /// Wording of the line-table item-type column (`Item type`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub item_type: String,
    /// Wording of the payments/settlements section (`Payments`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub payments: String,
    /// Wording of the amount-due row (`Amount due`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub amount_due: String,
}

impl LabelOverrides {
    /// Resolve a label, falling back to the built-in (trimmed).
    #[must_use]
    pub fn label<'a>(&'a self, value: &'a str, fallback: &'a str) -> &'a str {
        let v = value.trim();
        if v.is_empty() { fallback } else { v }
    }

    pub fn validate(&self, errors: &mut Vec<FieldError>) {
        for (field, v) in [
            ("labels.description", &self.description),
            ("labels.quantity", &self.quantity),
            ("labels.unit_price", &self.unit_price),
            ("labels.subtotal", &self.subtotal),
            ("labels.discount", &self.discount),
            ("labels.tax", &self.tax),
            ("labels.total", &self.total),
            ("labels.from", &self.from),
            ("labels.invoice_for", &self.invoice_for),
            ("labels.invoice_id", &self.invoice_id),
            ("labels.issue_date", &self.issue_date),
            ("labels.due_date", &self.due_date),
            ("labels.subject", &self.subject),
            ("labels.payment_heading", &self.payment_heading),
            ("labels.item_type", &self.item_type),
            ("labels.payments", &self.payments),
            ("labels.amount_due", &self.amount_due),
        ] {
            if v.chars().count() > 40 {
                errors.push(FieldError::new(field, "label too long (max 40 characters)"));
            }
        }
    }
}

// ---------------------------------------------------------------- inputs ---

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CustomerInput {
    pub name: String,
    #[serde(default)]
    pub invoice_prefix: Option<String>,
    pub currency: Currency,
    pub default_rate_minor: u64,
    #[serde(default = "default_active")]
    pub active: bool,
    #[serde(default)]
    pub email: String,
    #[serde(default)]
    pub payment_terms: Option<PaymentTerms>,
    #[serde(default)]
    pub invoice_notes: String,
    #[serde(default)]
    pub invoice_subject: String,
    #[serde(default)]
    pub address: Option<Address>,
    #[serde(default)]
    pub contacts: Vec<Contact>,
    #[serde(default)]
    pub tax_hundredths: u16,
    #[serde(default)]
    pub discount_hundredths: u16,
    #[serde(default)]
    pub payment_details: Option<PaymentDetails>,
    /// Company/registration number printed on documents (<= 60 chars).
    #[serde(default)]
    pub legal_id: String,
}

pub fn validate_customer_input(input: &CustomerInput) -> Result<CustomerDraft, Vec<FieldError>> {
    let mut errors = Vec::new();
    let invoice_prefix =
        normalize_invoice_prefix(input.invoice_prefix.as_deref().unwrap_or_default());
    if invoice_prefix.is_none() {
        errors.push(FieldError::new(
            "invoice_prefix",
            "at most 20 letters, digits or internal hyphens; INV is reserved",
        ));
    }
    let name = validate_name(&input.name).map(str::to_owned);
    if name.is_none() {
        errors.push(FieldError::new("name", "required, at most 120 characters"));
    }
    validate_rate_minor(input.default_rate_minor, "default_rate_minor", &mut errors);
    let email = input.email.trim();
    if !email.is_empty() && !crate::email::valid_address(email) {
        errors.push(FieldError::new("email", "not a valid email address"));
    }
    // Invoice document fields (#116): same gauntlet as the rest of the input,
    // including the template validators so unknown %tokens% never persist.
    validate_payment_terms(input.payment_terms.as_ref(), &mut errors);
    crate::template::validate_field(
        "invoice_notes",
        &input.invoice_notes,
        crate::template::NOTES_MAX,
        &mut errors,
    );
    crate::template::validate_field(
        "invoice_subject",
        &input.invoice_subject,
        crate::template::SUBJECT_MAX,
        &mut errors,
    );
    // #S1: the subject is header-bound, so a CR/LF/NUL in it could fold a
    // header into an email the app sends in the organisation's name. Refused on
    // write, like every other invalid shape -- never silently stripped.
    if crate::email::has_control_characters(&input.invoice_subject) {
        errors.push(FieldError::new(
            "invoice_subject",
            "must be a single line without control characters",
        ));
    }
    // A variable whose value spans lines is a document block, never a subject:
    // interpolating it would freeze a broken subject at issue.
    for token in crate::template::multiline_tokens(&input.invoice_subject) {
        errors.push(FieldError::new(
            "invoice_subject",
            format!(
                "cannot use %{token}% in the subject: its value spans multiple lines (use it in the body, footer or notes)"
            ),
        ));
    }
    // Customer company/registration number: bounded and printable (a control
    // character in a printed identity is never a valid shape).
    if input.legal_id.chars().count() > 60 {
        errors.push(FieldError::new("legal_id", "at most 60 characters"));
    }
    if crate::email::has_control_characters(&input.legal_id) {
        errors.push(FieldError::new(
            "legal_id",
            "must not contain control characters",
        ));
    }
    validate_customer_billing_fields(
        &input.address,
        &input.contacts,
        input.tax_hundredths,
        input.discount_hundredths,
        &input.payment_details,
        &mut errors,
    );
    if errors.is_empty() {
        Ok(CustomerDraft {
            name: name.unwrap(),
            invoice_prefix: invoice_prefix.unwrap_or_default(),
            currency: input.currency.0.clone(),
            default_rate_minor: input.default_rate_minor,
            active: input.active,
            email: email.to_string(),
            payment_terms: input.payment_terms,
            invoice_notes: input.invoice_notes.trim().to_string(),
            invoice_subject: input.invoice_subject.trim().to_string(),
            address: input.address.as_ref().map(|a| Address {
                street: a.street.trim().to_string(),
                city: a.city.trim().to_string(),
                postal_code: a.postal_code.trim().to_string(),
                country: a.country.trim().to_string(),
            }),
            contacts: input
                .contacts
                .iter()
                .map(|c| Contact {
                    name: c.name.trim().to_string(),
                    role: c.role.trim().to_string(),
                    email: c.email.trim().to_string(),
                    billing: c.billing,
                })
                .collect(),
            tax_hundredths: input.tax_hundredths,
            discount_hundredths: input.discount_hundredths,
            // An all-blank payment block normalises to "not configured" —
            // the Get-paid panel and emails then honestly omit it.
            payment_details: input
                .payment_details
                .as_ref()
                .filter(|p| p.has_content())
                .map(PaymentDetails::trimmed),
            legal_id: input.legal_id.trim().to_string(),
        })
    } else {
        Err(errors)
    }
}

/// `custom` requires days in 1..=365; every other kind must omit them (#116).
pub fn validate_payment_terms(terms: Option<&PaymentTerms>, errors: &mut Vec<FieldError>) {
    let Some(t) = terms else { return };
    match t.kind {
        TermsKind::Custom => match t.days {
            Some(d) if (1..=365).contains(&d) => {}
            _ => errors.push(FieldError::new(
                "payment_terms",
                "custom terms require days in 1..=365",
            )),
        },
        _ => {
            if t.days.is_some() {
                errors.push(FieldError::new(
                    "payment_terms",
                    "only custom terms carry a day count",
                ));
            }
        }
    }
}

/// Validated, ready-to-persist payloads (owned, normalised strings).
#[derive(Debug)]
pub struct CustomerDraft {
    pub name: String,
    pub invoice_prefix: String,
    pub currency: String,
    pub default_rate_minor: u64,
    pub active: bool,
    pub email: String,
    pub payment_terms: Option<PaymentTerms>,
    pub invoice_notes: String,
    pub invoice_subject: String,
    pub address: Option<Address>,
    pub contacts: Vec<Contact>,
    pub tax_hundredths: u16,
    pub discount_hundredths: u16,
    pub payment_details: Option<PaymentDetails>,
    pub legal_id: String,
}

pub fn normalize_invoice_prefix(value: &str) -> Option<String> {
    if value.chars().count() > 64 || value.chars().any(char::is_control) {
        return None;
    }
    if value.trim().is_empty() {
        return Some(String::new());
    }
    let prefix = value.trim().trim_end_matches('-').to_ascii_uppercase();
    if prefix.is_empty()
        || prefix.len() > 20
        || prefix == "INV"
        || !prefix
            .bytes()
            .all(|value| value.is_ascii_alphanumeric() || value == b'-')
        || !prefix
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
    {
        return None;
    }
    Some(prefix)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn customer_input(name: &str, currency: &str, rate: u64) -> CustomerInput {
        CustomerInput {
            name: name.into(),
            currency: Currency::parse(currency).unwrap(),
            default_rate_minor: rate,
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
    fn customer_validation_boundaries() {
        assert!(validate_customer_input(&customer_input("Tucano Time", "EUR", 6000)).is_ok());
        let errs = validate_customer_input(&customer_input("  ", "EUR", 6000)).unwrap_err();
        assert_eq!(errs[0].field, "name");
        let errs = validate_customer_input(&customer_input("x", "EUR", 100_000_001)).unwrap_err();
        assert_eq!(errs[0].field, "default_rate_minor");
    }

    #[test]
    fn unknown_fields_are_rejected_by_dto_deser() {
        let err = serde_json::from_str::<CustomerInput>(
            r#"{"name":"A","currency":"EUR","default_rate_minor":1,"extra":true}"#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("unknown field"));
    }

    #[test]
    fn legacy_customer_docs_parse_with_new_invoice_fields_defaulted() {
        // #116 backward compatibility: documents written before payment
        // terms / notes / subject existed must still deserialize (and the
        // absent terms must not break due-date resolution).
        let legacy = r#"{
            "id": "11111111-1111-1111-1111-111111111111",
            "name": "Old Co",
            "currency": "EUR",
            "default_rate_minor": 5000,
            "active": true
        }"#;
        let c: Customer = serde_json::from_str(legacy).unwrap();
        assert_eq!(c.payment_terms, None);
        assert_eq!(c.invoice_notes, "");
        assert_eq!(c.invoice_subject, "");
        // Round-trips without serialising empty optionals back out.
        let json = serde_json::to_string(&c).unwrap();
        assert!(!json.contains("payment_terms"), "{json}");
    }

    #[test]
    fn payment_terms_validation() {
        let mut errs = Vec::new();
        validate_payment_terms(
            Some(&PaymentTerms {
                kind: TermsKind::Custom,
                days: None,
            }),
            &mut errs,
        );
        assert_eq!(errs.len(), 1);
        validate_payment_terms(
            Some(&PaymentTerms {
                kind: TermsKind::Custom,
                days: Some(400),
            }),
            &mut errs,
        );
        assert_eq!(errs.len(), 2);
        validate_payment_terms(
            Some(&PaymentTerms {
                kind: TermsKind::Net30,
                days: Some(30),
            }),
            &mut errs,
        );
        assert_eq!(errs.len(), 3, "fixed kinds must omit days");
        validate_payment_terms(
            Some(&PaymentTerms {
                kind: TermsKind::Custom,
                days: Some(21),
            }),
            &mut errs,
        );
        validate_payment_terms(
            Some(&PaymentTerms {
                kind: TermsKind::UponReceipt,
                days: None,
            }),
            &mut errs,
        );
        assert_eq!(errs.len(), 3);
        assert_eq!(
            PaymentTerms {
                kind: TermsKind::UponReceipt,
                days: None
            }
            .fixed_days(),
            Some(0)
        );
        assert_eq!(
            PaymentTerms {
                kind: TermsKind::Net45,
                days: None
            }
            .fixed_days(),
            Some(45)
        );
        assert_eq!(
            PaymentTerms {
                kind: TermsKind::Custom,
                days: Some(9)
            }
            .fixed_days(),
            Some(9)
        );
    }

    /// A legacy `customers/<id>.json` (no `legal_id`) loads, defaults empty and
    /// round-trips byte-identically.
    #[test]
    fn legacy_customer_document_round_trips_byte_identically() {
        let legacy = r#"{"id":"11111111-1111-1111-1111-111111111111","name":"Old Co","currency":"EUR","default_rate_minor":5000,"active":true,"email":"","invoice_notes":"","invoice_subject":""}"#;
        let c: Customer = serde_json::from_str(legacy).unwrap();
        assert_eq!(c.legal_id, "");
        assert_eq!(serde_json::to_string(&c).unwrap(), legacy);
    }

    #[test]
    fn legal_id_is_bounded_and_printable() {
        let mut input = customer_input("Tucano Time", "EUR", 6000);
        input.legal_id = "1".repeat(61);
        let errs = validate_customer_input(&input).unwrap_err();
        assert_eq!(errs[0].field, "legal_id");
        input.legal_id = "122\r\nX".into();
        let errs = validate_customer_input(&input).unwrap_err();
        assert_eq!(errs[0].field, "legal_id");
        input.legal_id = " 99999999 ".into();
        let draft = validate_customer_input(&input).unwrap();
        assert_eq!(draft.legal_id, "99999999");
    }

    #[test]
    fn payment_rails_are_bounded_and_validated() {
        let rail = |currency: &str, iban: Option<&str>, bic: Option<&str>| PaymentRail {
            currency: currency.into(),
            account_name: "Holder".into(),
            iban: iban.map(str::to_string),
            bic: bic.map(str::to_string),
            ..Default::default()
        };
        let mut errors = Vec::new();
        validate_customer_billing_fields(
            &None,
            &[],
            0,
            0,
            &Some(PaymentDetails {
                rails: vec![rail("", None, None)],
                ..Default::default()
            }),
            &mut errors,
        );
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].field, "payment_details.rails[0].currency");
        // A blank account name is refused: the rail names no account holder.
        let mut errors = Vec::new();
        validate_customer_billing_fields(
            &None,
            &[],
            0,
            0,
            &Some(PaymentDetails {
                rails: vec![PaymentRail {
                    currency: "EUR".into(),
                    account_name: "   ".into(),
                    ..Default::default()
                }],
                ..Default::default()
            }),
            &mut errors,
        );
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].field, "payment_details.rails[0].account_name");
        let mut errors = Vec::new();
        validate_customer_billing_fields(
            &None,
            &[],
            0,
            0,
            &Some(PaymentDetails {
                rails: vec![rail("EUR", Some("AB"), Some("SAMPLE"))],
                ..Default::default()
            }),
            &mut errors,
        );
        assert!(
            errors
                .iter()
                .any(|e| e.field == "payment_details.rails[0].iban")
        );
        assert!(
            errors
                .iter()
                .any(|e| e.field == "payment_details.rails[0].bic")
        );
        let mut errors = Vec::new();
        validate_customer_billing_fields(
            &None,
            &[],
            0,
            0,
            &Some(PaymentDetails {
                rails: (0..=MAX_PAYMENT_RAILS)
                    .map(|_| rail("EUR", None, None))
                    .collect(),
                ..Default::default()
            }),
            &mut errors,
        );
        assert!(errors.iter().any(|e| e.field == "payment_details.rails"));
        // A well-formed rail is accepted and normalises to upper-case.
        let mut errors = Vec::new();
        validate_customer_billing_fields(
            &None,
            &[],
            0,
            0,
            &Some(PaymentDetails {
                rails: vec![rail("EUR", Some("EX000000000000000000"), None)],
                ..Default::default()
            }),
            &mut errors,
        );
        assert!(errors.is_empty(), "{errors:?}");
        // Lower-case is a bad shape (the contract says upper-case ISO), but a
        // *stored* rail is normalised on the way in.
        assert_eq!(
            PaymentDetails {
                rails: vec![rail("eur", None, None)],
                ..Default::default()
            }
            .trimmed()
            .rails[0]
                .currency,
            "EUR"
        );
        assert_eq!(
            PaymentDetails {
                rails: vec![rail("eur", None, None)],
                ..Default::default()
            }
            .trimmed()
            .rails[0]
                .currency,
            "EUR"
        );
    }

    #[test]
    fn legacy_org_profile_round_trips_and_label_overrides_round_trip() {
        let legacy = r#"{"name":"Org","legal_id":"BE0123456789","address":null}"#;
        let profile: OrgProfile = serde_json::from_str(legacy).unwrap();
        assert_eq!(profile.legal_id_label, "");
        assert_eq!(serde_json::to_string(&profile).unwrap(), legacy);

        let labels = LabelOverrides {
            from: "From".into(),
            invoice_for: "Invoice for".into(),
            invoice_id: "Invoice ID".into(),
            issue_date: "Issue date".into(),
            due_date: "Due date".into(),
            subject: "Subject".into(),
            payment_heading: "Payment details".into(),
            item_type: "Item type".into(),
            payments: "Payments".into(),
            amount_due: "Amount due".into(),
            ..Default::default()
        };
        let json = serde_json::to_string(&labels).unwrap();
        assert_eq!(
            serde_json::from_str::<LabelOverrides>(&json).unwrap(),
            labels
        );
        let mut errors = Vec::new();
        LabelOverrides {
            item_type: "x".repeat(41),
            ..labels
        }
        .validate(&mut errors);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].field, "labels.item_type");
    }
}
