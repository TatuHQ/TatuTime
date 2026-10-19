use super::customer::{LabelOverrides, PaymentTerms, TermsKind};
use super::invoice::LineItemKind;
use super::primitives::{Currency, default_active};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A reusable Product/Service line type (#147), stored `items/<id>.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ItemType {
    pub id: Uuid,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub kind: LineItemKind,
    /// Default unit price in minor units (0 = ask at invoice time).
    pub default_price_minor: u64,
    #[serde(default = "default_currency_eur")]
    pub currency: Currency,
    #[serde(default = "default_active")]
    pub active: bool,
    pub created_at: DateTime<Utc>,
}

fn default_currency_eur() -> Currency {
    Currency("EUR".to_string())
}

/// Create/update payload for item types (#147).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ItemTypeInput {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub kind: LineItemKind,
    #[serde(default)]
    pub default_price_minor: u64,
    #[serde(default)]
    pub currency: Option<Currency>,
    #[serde(default = "default_active")]
    pub active: bool,
}

/// The org-wide invoice document template (#116), stored as the singleton
/// `invoice_template.json` next to `scheduler.json`. Every field is optional:
/// an unset template keeps the hard-coded legacy behaviour byte-for-byte.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvoiceTemplate {
    /// Subject line; `%variable%` placeholders resolved at render/issue.
    #[serde(default)]
    pub subject: String,
    /// Markdown-subset body shown before the line table.
    #[serde(default)]
    pub body: String,
    /// Markdown-subset footer (e.g. bank details), after the totals and
    /// before the `Notes` section.
    #[serde(default)]
    pub footer: String,
    /// Org-wide free text for the document's `Notes` section, printed under
    /// its heading above the customer's own notes and the payment details.
    /// Markdown subset + `%variables%`, like `footer`. Not seeded, so a fresh
    /// install keeps its historical bytes.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub notes: String,
    /// Default terms for customers that carry none.
    #[serde(default)]
    pub payment_terms: Option<PaymentTerms>,
    /// Document display labels (#147); empty = built-in.
    #[serde(default)]
    pub labels: LabelOverrides,
}

/// The subject a fresh install's template carries: the billing period, not
/// the issue date, e.g. `Tucano Time invoice for 08 2026`.
pub const DEFAULT_TEMPLATE_SUBJECT: &str =
    "%customer_name% invoice for %period_month_padded% %period_year%";

impl InvoiceTemplate {
    /// The org-wide defaults for an install that has never saved a template:
    /// `net_30` (the confirmed org default) and the period-based subject.
    ///
    /// Only the *absent singleton* resolves to this; an explicitly saved
    /// template is used exactly as stored, and the code fallback chain in
    /// `resolve_due_date` (customer terms → template terms → period_to + 14)
    /// is untouched.
    #[must_use]
    pub fn seeded() -> Self {
        Self {
            subject: DEFAULT_TEMPLATE_SUBJECT.to_string(),
            payment_terms: Some(PaymentTerms {
                kind: TermsKind::Net30,
                days: None,
            }),
            ..Self::default()
        }
    }
}
