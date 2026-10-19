# Architecture decision records

Decision records capture why the system is shaped the way it is. They are
historical by nature: the decision and its context are dated, and a later record
may supersede an earlier one.

| ADR | Title | Status |
| --- | --- | --- |
| [ADR-001](adr-001-filesystem-and-seams.md) | Filesystem storage, entity ownership, and integration seams | Accepted |
| [ADR-002](adr-002-einvoicing-scope.md) | E-invoicing standard and provider scope | Accepted (decision record; implementation is follow-up work) |
| [ADR-003](adr-003-entry-retention-and-archival.md) | Retention and archival for `entries/<YYYY-MM-DD>` | Accepted (cold tree, id index, read-path merge and per-run manifest implemented — #349/#350; automatic trigger and CLI/API invocation deferred) |

For the current storage layout and invariants, use
[Storage and retention](../storage-and-retention.md). This index describes
status, not live behaviour.
