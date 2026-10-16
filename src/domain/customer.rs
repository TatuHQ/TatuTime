use super::primitives::{
    Currency, FieldError, default_active, is_zero_u16, validate_name, validate_rate_minor,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Customer {
    pub id: Uuid,
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
/// details.iban = "DE89 3704 0044 0532 0130 00".into();
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

    /// Plain-text block for emails and the `%payment_details%` document
    /// variable: rows first, instructions last. `None` when empty so
    /// senders can omit the section honestly.
    #[must_use]
    pub fn email_block(&self) -> Option<String> {
        let mut lines = Vec::new();
        for (label, value) in self.rows() {
            lines.push(format!("{label}: {value}"));
        }
        if !self.instructions.trim().is_empty() {
            lines.push(self.instructions.trim().to_string());
        }
        if lines.is_empty() {
            None
        } else {
            Some(lines.join("\n"))
        }
    }
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
}

pub fn validate_customer_input(input: &CustomerInput) -> Result<CustomerDraft, Vec<FieldError>> {
    let mut errors = Vec::new();
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
            address: None,
            contacts: vec![],
            tax_hundredths: 0,
            discount_hundredths: 0,
            payment_details: None,
        }
    }

    #[test]
    fn customer_validation_boundaries() {
        assert!(validate_customer_input(&customer_input("ACME", "EUR", 6000)).is_ok());
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
}
