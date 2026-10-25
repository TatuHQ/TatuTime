# ADR-003: Retention and archival for `entries/<YYYY-MM-DD>`

Status: **accepted**. Implemented: the cold-tree move, cold id index, read-path merge and per-run sha256 manifest (#349/#350). Deferred: any automatic trigger or cutoff, and any CLI/API invocation — the pass is the library method `Store::archive_entries_before`, exercised by tests today.
Deciders: Emanuele Ciurleo (owner), Maintainers
Date: 2026-10-16
Applies to: #337 (this record), #94 (backup), #335 (per-user read indexes), #336 (storage-layout guard), #52 (audit trail)

## Context

**One writer, one document per day.** An entry is one pretty-printed JSON document
under a per-day folder, `entries/<YYYY-MM-DD>/<id>.json`, written by
`Store::put_entry` in `src/store/entries.rs` under the single write lock
(`Store::write_lock` in `src/store/mod.rs`). Nothing condenses or expires a day:
the only startup hygiene is `*.tmp` pruning (`prune_tmp_files` in
`src/store/io.rs`) — a crashed write, not retention; `rebuild_read_indexes`
(`src/store/index.rs`) rebuilds an *index*, it removes nothing; and the only
removal is the explicit `Store::delete_entry` (`src/store/entries.rs`) behind a
user deleting a row. An installation therefore accumulates one day folder per day
it was used, forever.

**Reads are bounded — except the three that matter.** The query window is capped at
`MAX_RANGE_DAYS = 400` (`src/store/mod.rs`), enforced on `Store::list_range`
(`src/store/entries.rs`) and `Store::list_range_for_user` (`src/store/index.rs`),
so a timesheet or calendar query never walks the tree. Three reads are deliberately
unbounded: `Store::get_entry(id)` probes **every** day folder because the id→path
address is not derivable from the id (`src/store/entries.rs`; the #190 note records
that the claimed range bound was never implemented), `Store::scan_all_entries`
decodes **every** entry ever (`src/store/entries.rs`) behind `Store::list_all_entries`,
and budgets are lifetime totals built on exactly that scan (`src/budgets.rs`).

`MAX_DOCS = 100_000` (`src/store/mod.rs`) is the store's compile-time
*per-directory* cap; the server boot path overrides it with the whitelisted
`max_docs` value (`TATU_MAX_DOCS` or `config.json`, default 50 000, range
10–1,000,000). `dir_entries` (`src/store/io.rs`) skips dot files and applies the
cap to the documents it lists. On `entries/` the directory holds day folders, not
documents, so the cap is ~274 years of days; inside one day folder it is 100 000
entries for that day. **The cap is not what limits growth — elapsed days are.**

**The #335 read indexes keep a slice per day.** `entries/.idx.users.json` is
`{ "<YYYY-MM-DD>": { "<user>": ["<id>", …] } }` (`src/store/index.rs`), with the
per-user variant for expenses (`src/store/index.rs`). Both are dot files, so
`dir_entries` (`src/store/io.rs`) skips them — never counted toward `MAX_DOCS`,
never returned by a listing (see the storage-layout section of `AGENTS.md`). Each
is written under the same write lock as the document it files
(`src/store/index.rs`), and the trust rule keeps a stale index merely *slower*,
never wrong: the index's id set must equal the id set of the `<id>.json` files on
disk, else the read falls back to the authoritative scan and heals
(`src/store/index.rs`). Retention's
consequence: the index carries one date key — and one slice per user who worked that
day — for every day the instance has ever seen, so relocating documents without
rewriting it leaves it growing behind them. The #335 PR body flagged precisely this:
a relocated day keeps a stale slice whose id set no longer matches the absent folder,
forcing a miss and a rescan.

**Backups take the tree byte for byte.** `--backup` archives the entire data dir as
a plain `tar.gz` plus a `manifest.json` recording every file's size and sha256
(`src/backup.rs`, the `Manifest`/`FileEntry` types and the `create` builder);
`walk` skips dot *directories* and `*.tmp`/`*.lock` but keeps dot files such as
`invoices/.seq.json` (`src/backup.rs`, the `walk` function). Backup and restore
cost is therefore linear in the document count — retention and backup time are the
same equation.

**What may never move.** The lock seam (#18/#185) defines what is still live. An
entry referenced by a **non-draft** invoice is frozen permanently: `InvoiceLock`
answers from the memoized `LockSnapshot` (`src/lock.rs`, `src/store/lock.rs`), and
a submitted/approved week freezes through `SubmissionLock` (`src/lock.rs`). All
providers compose behind `CombinedLocks`, and an unverifiable lock state refuses
fail-closed (`src/lock.rs`). The snapshot is keyed on the source collections'
directory mtimes (#255, `Store::lock_snapshot` in `src/store/lock.rs`), so moving
documents between collections is observable to it. `domain::already_invoiced`
(`src/domain/invoice_gen.rs`) names the complementary set: every entry on a
*non-draft* invoice, whether or not it is still locked.

The rule this ADR records as non-negotiable: **an entry referenced by a non-draft
invoice, or sitting in a submitted/approved week, must never be archived, moved or
rewritten.** Issuing an invoice freezes its entries and expenses, and settling
(`paid`) or writing off (`written_off`) does not release them (`src/lock.rs`,
`src/store/lock.rs`, recorded in `AGENTS.md` and ADR-001). The archival predicate
therefore excludes *any* non-draft invoice reference, not merely the currently open
ones.

## Cost of doing nothing

Measured from a real document in the developer's data dir,
`data/entries/2026-10-09/1ffa5937-….json`: 14 lines, ≈ 0.4 KB. At a plausible
~4 entries per working day and ~220 working days a year (≈ 880 documents/year/active
user):

| Horizon | Documents per active user | ≈ bytes per active user |
| --- | --- | --- |
| 1 year | ~880 | ~0.35 MB |
| 3 years | ~2 640 | ~1.05 MB |
| 5 years | ~4 400 | ~1.75 MB |

The bytes are trivial. What grows is the **count of day folders** and the size of the
index map: 5 years is ~1 826 day folders per installation *regardless of headcount*,
and each is one `read_dir` probe on every `get_entry` (`src/store/entries.rs`) and
one date key in `entries/.idx.users.json` (`src/store/index.rs`). Bytes scale with
users × time; days scale with time alone. The honest cost of doing nothing is a slow
id-addressed read and a slow lifetime scan that both grow with the *age* of the
instance, plus a backup whose file count grows the same way — not a disk-space
problem and not a `MAX_DOCS` problem.

## Options considered

| Option | What it buys | What it threatens |
| --- | --- | --- |
| **A. Do nothing** | Zero code; the layout stays the single source of truth and every `AGENTS.md` claim stays true | Linear-in-age growth of id-addressed reads (`src/store/entries.rs`), lifetime scans (`src/store/entries.rs`), the index map (`src/store/index.rs`) and backup file count (`src/backup.rs`) |
| **B. Archive by age to a cold tree** (`archive/entries/<YYYY-MM>/…`) with a manifest | The hot tree stops growing; cold data stays human-readable and inside the same backup | (1) the #336 guard reads the `AGENTS.md` "Storage layout" bullet (`tests/contract/store.rs`, `src/store/mod.rs`), so the tree must be added to `DOC_COLLECTIONS` (`src/store/mod.rs`) *and* the bullet in one change or the build fails; (2) a move outside the write lock (`src/store/mod.rs`) races a concurrent `put_entry`; (3) reads must merge hot+cold or explicitly refuse — a silent split changes lifetime scans, and budgets are lifetime totals (`src/budgets.rs`) |
| **C. Export then delete** (a backup plus a tombstone) | Shrinks the live tree hardest; the export is the record | The filesystem stops being the store for the exported ids: `get_entry` can no longer address them (`src/store/entries.rs`); the tombstone duplicates the audit trail (#52) without its guarantees; a settled invoice's rate/hours evidence leaves the live tree even though the invoice remains final evidence (`src/lock.rs`) |
| **D. Compact a month into one document** (`entries/<YYYY-MM>.json`) | Fewest files; day folders collapse | Breaks the one-document-per-`Uuid`, date-sharded path builders the store derives everything from (`src/store/entries.rs`); the #335 index assumes day keys (`src/store/index.rs`); one row edit becomes a read-modify-write of a whole month, and tmp+rename protects one document, not a merge of many — blast radius grows from one entry to a month |
| **E. Queryable detached partition** (`entries-cold/`, served by the API) | Cold data stays queryable and hot reads stay fast | The most code for the least product: a second root, a merge layer over `get_entry`/`list_range`/`scan_all_entries`, a second `MAX_DOCS` accounting and a second per-user index — the highest fork risk in a file-based product with no database |

The money/hours invariants are safe in every option provided a move copies bytes
without decoding and re-encoding (`u64` minor units, `u32` hundredths — ADR-001 §2)
and uses the store's tmp+rename discipline rather than `mv`.

## Decision

**Defer the trigger, not the primitive.** No archival job runs automatically today:
the tree is small at this product's design point (a small team billing its own
tracked work), and the trigger below is what would justify invoking the pass. The
store primitive and the read story are implemented (#349/#350); what remains
deferred is the invocation policy — no schedule, no default cutoff and no CLI or
HTTP entry point.

**Trigger** — measured on the live data dir with stock tools; any one is sufficient:

```sh
du -sh "$TATU_DATA_DIR/entries"        # > 1 GiB
ls "$TATU_DATA_DIR/entries" | wc -l    # > 3 650 day folders (~10 years)
```

or, observed at runtime, a `list_all_entries`-backed request
(`src/store/entries.rs`, the admin budget report) or a `--backup` run exceeding
~60 s. Each means the
count/time cost has outgrown the design point; below them an archival job is churn
with a correctness risk and no payoff.

**What happens when it fires — today:** the pass is the library method
`Store::archive_entries_before(cutoff, dry_run, locks)`; there is no CLI command, so
invoking it is a developer operation (the contract tests are its only caller today).
A future operator command would call it with `dry_run = true` first, read the report
(days, documents, bytes, and the entries *skipped* for being locked or
invoice-referenced), take a `--backup`, then run it for real and keep the manifest
with that backup.

**What is archived, and how integrity is proven.** A whole day folder's `<id>.json`
documents — the same bytes moved, never decoded and re-encoded — plus that day's slices
in `entries/.idx.users.json` (`src/store/index.rs`), which must leave the hot index
because it may only name days that are on disk. A per-run manifest of sha256s, the
shape the backup `Manifest`/`FileEntry` rows already use (`src/backup.rs`) to prove a
#113 archived PDF survived, is what a restore verifies against.

**Preconditions — built; the trigger is now actionable:**

1. (#349, **implemented** — `Store::archive_entries_before` in `src/store/cold.rs`) The
   primitive (a) takes the single write lock for the whole pass (`src/store/mod.rs`),
   (b) **refuses** any day holding an entry that is locked (`CombinedLocks`,
   `src/lock.rs`) or on any non-draft invoice (`domain::already_invoiced`,
   `src/domain/invoice_gen.rs`), (c) moves whole day folders by byte copy with
   tmp+rename rather than `mv`, (d) writes a per-run manifest of sha256s in the shape
   `src/backup.rs` already uses, and (e) drops the relocated days' keys from
   `entries/.idx.users.json` (`src/store/index.rs`) so the index shrinks with the
   documents.
2. (#350, **implemented — see "Read story (#350)" below**) Every reader merges
   the cold tree: `list_by_date`/`list_range`/`get_entry` and the lifetime
   `scan_all_entries` answer from hot ∪ cold, so the lifetime scans behind budgets
   (`src/budgets.rs`) keep seeing cold data. The alternative branch — declaring
   budget totals hot-only — is explicitly **not** taken.
3. (#336, **implemented**) The guard is kept honest: `ARCHIVE_DIR` is listed in
   `DOC_COLLECTIONS` (`src/store/mod.rs`) and the cold tree is part of the
   `AGENTS.md` storage-layout bullet.

**Out of scope (still deferred):** a schedule, a default cutoff, any automatic
deletion and any CLI or HTTP entry point. The primitive is a developer/API-only
operation today: `Store::archive_entries_before(cutoff, dry_run, locks)` is a
library method exercised by the contract tests, and `src/main.rs` exposes only
`--backup`, `--restore`, `--health` and `--version`. Nothing invokes the pass
automatically.

## Read story (#350)

Precondition 2, in the form it was implemented: **archiving relocates bytes and changes no
number any caller or user can see.** The cold tree is the same documents in a different
folder, so the read story is a merge, not a second store.

- **The merge.** `list_by_date` answers from the merged day — the hot day folder plus the
  cold day folder — and every reader built on it (`list_range`, `list_range_for_user`,
  `list_by_date_for_user`) inherits it unchanged. `get_entry` keeps its hot walk and
  resolves the cold copy on a miss. `scan_all_entries` — and therefore
  `list_all_entries`, the budget report and the deletion guards built on it — walks the
  hot day folders and then the cold ones, under **one** `MAX_DOCS` budget over the union.
- **Hot wins the deduplication.** The merge is by entry id and the hot copy wins. The
  #349 pass removes the hot day only after its cold copy is complete, so a crash can leave
  a day in both trees; resolving to the hot copy means such a day reads exactly as it read
  before the pass, and no document is ever counted or returned twice.
- **Rehydration.** A cold entry is writable by construction (#349 refuses locked and
  invoiced days), so every write — `put_entry`, `save_entry_inner` (both the old and the
  new date, so moving out of or between cold days strands nothing), `delete_entry` and
  `delete_entry_guarded` — removes the cold copy and leaves exactly one document. The
  existence check behind a delete spans both trees, so an entry `get_entry` resolved is
  never a 404 on DELETE. A cold day folder emptied by a write is pruned best-effort.
- **The #335 per-user index compares against the merged day.** #349 prunes the relocated
  day's key, which makes the index's id set (empty) equal the hot folder's id set (empty):
  comparing against the hot folder alone would therefore *validate* an empty read and show
  a member their own archived week as zero hours with no error. The comparison is against
  hot ∪ cold, and the opportunistic heal files **hot ids only** — filing the merged set
  would re-add a key naming a day that is not on disk and regress #349's invariant.
- **Cold id index.** `archive/entries/.idx.ids.json` (`date -> ids`; a dot file, not a
  document) is written **only** by the archival pass, in its end-of-pass block. A read
  consults it as a *hint* for point reads, validates a slice against the cold day folder
  before believing it, and falls back to the authoritative walk when the index is absent,
  truncated or drifted. Correctness never depends on it, and no read writes it.
- **Enumeration.** The cold tree is listed with the same `dir_entries` helper as every
  other walk, so the run manifests, the cold id index and a pass's `.<day>.staging`
  leftovers can never be mistaken for a day folder or count toward `MAX_DOCS`.

**Accepted limitation.** Rehydrating an entry — or deleting one — leaves the manifest of
the run that archived it naming a file that is no longer under `archive/entries/`: a
relocation back while the id is hot, a loss otherwise. The manifest is the operator's
restore record, no reader consults it, and a verifier for it is deliberately not built
here.

**Guarded by** `tests/contract/store.rs::cold_tree_reads`, with the API-level cases in
`tests/contract/entries_reports.rs` and `tests/contract/invoicing.rs`: a cold day reads by
date, range and id; the lifetime scan counts hot and cold exactly once, hot winning; budgets
and the `list_all_entries`-backed deletion guards still see archived history; a member's own
archived day is not emptied (and the heal adds no key naming the cold day); an edit, a date
move or a delete rehydrates without duplicating, the second delete being a 404; a
half-published day reads without error or duplicates; the cold index is never the source of
truth; and an archived unbilled period still previews and issues.

## Consequences

**Harder:**

- Restores become two-step (the archive is a second tree to place back), and the
  sha256 manifest habit (the backup `FileEntry` rows in `src/backup.rs`) is what
  proves a move was lossless.
- An invoice referencing an archived day stays legal evidence, so the safety check
  consults invoices forever rather than once.
- `audit.log` (#52) records the mutation but not the file move; the per-run manifest
  is what makes a relocation auditable.
- `entries/.idx.users.json` must be rewritten in the same pass or the days it still
  names force a rescan on every member read (`src/store/index.rs`) — #335's own flag.
- The #336 guard turns "add a cold tree" into a two-file change (code list +
  `AGENTS.md`), deliberately.

**Unchanged:**

- The date-sharded layout, one document per `Uuid`, atomic temp+rename writes and
  `MAX_DOCS`: an archived day is the same JSON in a different folder.
- Money and hours stay minor units and hundredths — a byte-copying move never parses them.
- The lock seam and `domain::already_invoiced` remain the sole authority on what may
  move; no retention code may invent its own predicate.
- Normal queries were already bounded to 400 days (`src/store/mod.rs`).
- The GUI reads no storage and needs no change.

## Follow-up work

1. **#349 — Store-owned archival primitive: implemented.** `Store::archive_entries_before`
   (`src/store/cold.rs`) takes the write lock for the whole pass, refuses locked,
   invoiced or unverifiable days, moves whole day folders byte for byte, writes the
   per-run sha256 manifest, drops the relocated keys from `entries/.idx.users.json`
   and refreshes `archive/entries/.idx.ids.json`.
2. **#350 — Cold-tree read story: implemented** — see "Read story (#350)": the merge
   into `list_by_date`/`list_range`/`get_entry`/`scan_all_entries`, the hot-wins
   deduplication, rehydration on write, the merged #335 trust rule and the pass-written
   cold id index. A relocation no longer changes what queries and budget totals see.
3. **Still deferred:** a schedule, a default cutoff, automatic deletion and any CLI or
   HTTP entry point. There is no `--archive` command; the pass is a library method
   exercised by the contract tests.
