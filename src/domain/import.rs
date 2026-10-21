use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{Currency, Customer, Entry, Expense, Hours, Project, effective_rates};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportProvider {
    Harvest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportOrigin {
    pub provider: ImportProvider,
    pub namespace: String,
    pub batch_id: Uuid,
    pub source_key: String,
    pub invoiced: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportedTime {
    pub origin: ImportOrigin,
    pub currency: Currency,
    pub rate_minor: u64,
    pub cost_rate_minor: u64,
    pub source_hours: Hours,
    pub amount_minor: u64,
    pub cost_amount_minor: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportedInvoice {
    pub origin: ImportOrigin,
    pub original_number: String,
    pub pdf_sha256: String,
    pub opening_paid_minor: u64,
}

impl Entry {
    pub fn externally_invoiced(&self) -> bool {
        self.imported
            .as_ref()
            .is_some_and(|imported| imported.origin.invoiced)
    }

    pub fn billing_rates(
        &self,
        customer: &Customer,
        project: Option<&Project>,
        user_rate: Option<u64>,
    ) -> (Currency, u64) {
        self.imported
            .as_ref()
            .map(|imported| (imported.currency.clone(), imported.rate_minor))
            .unwrap_or_else(|| effective_rates(customer, project, user_rate))
    }

    pub fn billing_amount_minor(&self, rate_minor: u64) -> u64 {
        match &self.imported {
            Some(imported) if self.hours == imported.source_hours => imported.amount_minor,
            Some(imported) => self.hours.amount_minor(imported.rate_minor),
            None => self.hours.amount_minor(rate_minor),
        }
    }

    pub fn imported_cost_minor(&self, fallback_rate: u64) -> u64 {
        match &self.imported {
            Some(imported) if self.hours == imported.source_hours => imported.cost_amount_minor,
            Some(imported) => self.hours.amount_minor(imported.cost_rate_minor),
            None => self.hours.amount_minor(fallback_rate),
        }
    }
}

impl Expense {
    pub fn externally_invoiced(&self) -> bool {
        self.imported.as_ref().is_some_and(|origin| origin.invoiced)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Invoice, InvoicePayment, InvoiceSources, InvoiceStatus, generate_invoice};
    use serde_json::json;

    fn origin(invoiced: bool) -> ImportOrigin {
        ImportOrigin {
            provider: ImportProvider::Harvest,
            namespace: "test".into(),
            batch_id: Uuid::new_v4(),
            source_key: "row:1".into(),
            invoiced,
        }
    }

    fn entry() -> Entry {
        serde_json::from_value(json!({"id":Uuid::new_v4(),"date":"2026-01-01","customer_id":Uuid::new_v4(),"project_code":"P1","hours":2,"note":"Work","billable":true,"created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z"})).unwrap()
    }

    fn customer(id: Uuid) -> Customer {
        serde_json::from_value(json!({"id":id,"name":"Example","currency":"EUR","default_rate_minor":9999,"active":true,"email":""})).unwrap()
    }

    fn snapshot(invoiced: bool) -> ImportedTime {
        ImportedTime {
            origin: origin(invoiced),
            currency: Currency("USD".into()),
            rate_minor: 0,
            cost_rate_minor: 5000,
            source_hours: Hours(200),
            amount_minor: 0,
            cost_amount_minor: 10000,
        }
    }

    #[test]
    fn legacy_records_omit_provenance_and_keep_existing_rate_resolution() {
        let entry = entry();
        assert!(entry.imported.is_none());
        assert!(
            serde_json::to_value(&entry)
                .unwrap()
                .get("imported")
                .is_none()
        );
        assert_eq!(
            entry.billing_rates(&customer(entry.customer_id), None, Some(1234)),
            (Currency("EUR".into()), 1234)
        );
        assert_eq!(entry.billing_amount_minor(1234), 2468);
        assert!(!entry.externally_invoiced());
    }

    #[test]
    fn imported_zero_rate_and_currency_override_current_person_defaults() {
        let mut entry = entry();
        entry.imported = Some(snapshot(false));
        assert_eq!(
            entry.billing_rates(&customer(entry.customer_id), None, Some(9999)),
            (Currency("USD".into()), 0)
        );
        assert_eq!(entry.billing_amount_minor(9999), 0);
        assert_eq!(entry.imported_cost_minor(9999), 10000);
        entry.hours = Hours(75);
        assert_eq!(entry.billing_amount_minor(9999), 0);
        assert_eq!(entry.imported_cost_minor(9999), 3750);
    }

    #[test]
    fn generation_excludes_external_history_without_a_local_invoice() {
        let mut historical = entry();
        historical.imported = Some(snapshot(true));
        let mut pending = historical.clone();
        pending.id = Uuid::new_v4();
        pending.imported.as_mut().unwrap().origin.invoiced = false;
        let entries = [historical, pending];
        let customer = customer(entries[0].customer_id);
        let sources = InvoiceSources {
            projects: &[],
            users: &[],
            entries: &entries,
            expenses: &[],
            excluded_entries: &[],
            excluded_expenses: &[],
            include_expenses: true,
            restrict_projects: false,
        };
        let invoice = generate_invoice(
            "_".into(),
            &customer,
            &sources,
            entries[0].date,
            entries[0].date,
            chrono::Utc::now(),
        )
        .unwrap();
        assert_eq!(invoice.lines.len(), 1);
        assert_eq!(invoice.lines[0].entry_id, Some(entries[1].id));
        assert_eq!(invoice.currency.0, "USD");
        assert_eq!(invoice.total_minor, 0);
    }

    #[test]
    fn imported_opening_settlement_is_not_a_fabricated_payment_ledger() {
        let mut invoice: Invoice = serde_json::from_value(json!({"id":Uuid::new_v4(),"number":"EX-1","customer_id":Uuid::new_v4(),"currency":"EUR","period_from":"2026-01-01","period_to":"2026-01-01","lines":[],"total_minor":10000,"status":"partly_paid","created_at":"2026-01-01T00:00:00Z"})).unwrap();
        invoice.imported = Some(ImportedInvoice {
            origin: origin(true),
            original_number: "EX-1".into(),
            pdf_sha256: "a".repeat(64),
            opening_paid_minor: 4000,
        });
        assert!(invoice.payments.is_empty());
        assert_eq!(invoice.paid_minor(), 4000);
        assert_eq!(invoice.balance_minor(), 6000);
        invoice.payments.push(InvoicePayment {
            id: Uuid::new_v4(),
            amount_minor: 1000,
            received_at: chrono::Utc::now(),
            reference: String::new(),
            method: "manual".into(),
        });
        assert_eq!(invoice.paid_minor(), 5000);
        assert_eq!(invoice.balance_minor(), 5000);
        invoice.status = InvoiceStatus::WrittenOff;
        assert_eq!(invoice.balance_minor(), 0);
        assert_eq!(invoice.paid_minor(), 5000);
    }
}
