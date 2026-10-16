//! Domain model and validation for TucanoTime.
//!
//! Money is stored in minor units (cents) as integers; hours are stored as
//! hundredths of an hour. Floats never touch persisted values, so totals are
//! exact and a future invoice snapshot cannot drift.

mod catalog;
mod claim;
mod customer;
mod entry;
mod expense;
mod invoice;
mod invoice_gen;
mod notification;
mod primitives;
mod project;
mod recurring;

pub use self::catalog::*;
pub use self::claim::*;
pub use self::customer::*;
pub use self::entry::*;
pub use self::expense::*;
pub use self::invoice::*;
pub use self::invoice_gen::*;
pub use self::notification::*;
pub use self::primitives::*;
pub use self::project::*;
pub use self::recurring::*;
