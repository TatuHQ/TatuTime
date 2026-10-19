// Contract tests: exercise the HTTP surface in-process against a temporary
// data dir, and check payloads against the checked-in openapi.json. This is
// the guard the shared rules demand: valid and invalid shapes, rejection
// before persistence, documented error shape, and no server-side leakage.

mod accounting;
mod auth;
mod calendar;
mod compose;
mod contract_parity;
mod customers_projects;
mod email;
mod entries_reports;
mod expenses;
mod fx;
mod gui_assets;
mod invoicing;
mod locking;
mod misc;
mod payments;
mod rbac;
mod retainers;
mod scheduler;
mod sso;
mod store;
mod submissions;
mod support;
mod tasks;
mod templates;
mod vault_config;
