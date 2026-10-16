# ADR-003: Retention and archival for `entries/<YYYY-MM-DD>`

Status: **accepted** (decision: defer to a measurable trigger; implementation is follow-up tickets)
Deciders: Emanuele Ciurleo (owner), AI agent
Date: 2026-10-16
Applies to: #337 (this record), #94 (backup), #335 (per-user read indexes), #336 (storage-layout guard), #52 (audit trail)

## Context

**One writer, one document per day.** An entry is one pretty-printed JSON document
under a per-day folder, `entries/<YYYY-MM-DD>/<id>.json` (`src/store.rs:1-9`),
written by `Store::put_entry` under the single write lock
(`src/store.rs:1877-1882`). Nothing condenses or expires a day: the only startup
hygiene is `*.tmp` pruning (`src/store.rs:302-311`, `prune_tmp_files` at
`src/store.rs:2402-2417`) — a crashed write, not retention; `rebuild_read_indexes`
(`src/store.rs:893`) rebuilds an *index*, it removes nothing; and the only removal
is the explicit `Store::delete_entry` (`src/store.rs:1884-1892`) behind a user
deleting a row. An installation therefore accumulates one day folder per day it
was used, forever.

**Reads are bounded — except the three that matter.** The query window is capped at
`MAX_RANGE_DAYS = 400` (`src/store.rs:29`), enforced on `list_range`
(`src/store.rs:1843-1846`) and `list_range_for_user` (`src/store.rs:934-942`), so a
timesheet or calendar query never walks the tree. Three reads are deliberately
unbounded: `get_entry(id)` probes **every** day folder because the id→path address
is not derivable from the id (`src/store.rs:1863-1874`; the #190 note records that
the claimed range bound was never implemented), `scan_all_entries` decodes **every**
entry ever (`src/store.rs:1930-1939`) behind `list_all_entries`
(`src/store.rs:1921-1928`), and budgets are lifetime totals built on exactly that
scan (`src/budgets.rs:125-127`).

`MAX_DOCS = 100_000` (`src/store.rs:39`) is a *per-directory* cap applied by
`dir_entries` (`src/store.rs:2432-2434`). On `entries/` the directory holds day
folders, not documents, so the cap is ~274 years of days; inside one day folder it
is 100 000 entries for that day. **The cap is not what limits growth — elapsed days
are.**

**The #335 read indexes keep a slice per day.** `entries/.idx.users.json` is
`{ "<YYYY-MM-DD>": { "<user>": ["<id>", …] } }` (`src/store.rs:605`), with the
per-user variant for expenses (`src/store.rs:606`). Both are dot files, so
`dir_entries` skips them — never counted toward `MAX_DOCS`, never returned by a
listing (`src/store.rs:2419-2437`, `AGENTS.md:64-67`). Each is written under the
same write lock as the document it files (`src/store.rs:610-613`), and the trust
rule keeps a stale index merely *slower*, never wrong: the index's id set must equal
the id set of the `<id>.json` files on disk, else the read falls back to the
authoritative scan and heals (`src/store.rs:615-628`, `695-708`). Retention's
consequence: the index carries one date key — and one slice per user who worked that
day — for every day the instance has ever seen, so relocating documents without
rewriting it leaves it growing behind them. The #335 PR body flagged precisely this:
a relocated day keeps a stale slice whose id set no longer matches the absent folder,
forcing a miss and a rescan.

**Backups take the tree byte for byte.** `--backup` archives the entire data dir as
a plain `tar.gz` plus a `manifest.json` recording every file's size and sha256
(`src/backup.rs:1-11`, `104-117`); `walk` skips dot *directories* and `*.tmp`/`*.lock`
but keeps dot files such as `invoices/.seq.json` (`src/backup.rs:61-93`). Backup and
restore cost is therefore linear in the document count — retention and backup time
are the same equation.

**What may never move.** The lock seam (#18/#185) defines what is still live. An
entry referenced by an **open** invoice (`issued | partly_paid`) is frozen:
`InvoiceLock` answers from the memoized `LockSnapshot` (`src/lock.rs:151-181`,
`src/store.rs:95-103`), and a submitted/approved week freezes through
`SubmissionLock` (`src/lock.rs:199-226`). All providers compose behind
`CombinedLocks`, and an unverifiable lock state refuses fail-closed (`src/lock.rs:95-149`).
The snapshot is keyed on the source collections' directory mtimes (#255,
`src/store.rs:2396-2400`, `lock_snapshot` at `src/store.rs:1467`), so moving
documents between collections is observable to it. `domain::already_invoiced`
(`src/domain.rs:1463-1466`) names the complementary set: every entry on a *non-draft*
invoice, whether or not it is still locked.

The rule this ADR records as non-negotiable: **an entry referenced by an open invoice,
or sitting in a submitted/approved week, must never be archived, moved or rewritten.**
Settled (`paid`) and written-off invoices deliberately release their entries
(`src/lock.rs:151-156`, recorded in `AGENTS.md` and ADR-001), so "unlocked" is the
right predicate for *mutation* — but a settled invoice's line is still the evidence
for a number that was issued, so the archival predicate must exclude *any* non-draft
invoice reference, not merely the currently locked ones.

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
and each is one `read_dir` probe on every `get_entry` (`src/store.rs:1863-1874`) and
one date key in `entries/.idx.users.json` (`src/store.rs:605`). Bytes scale with
users × time; days scale with time alone. The honest cost of doing nothing is a slow
id-addressed read and a slow lifetime scan that both grow with the *age* of the
instance, plus a backup whose file count grows the same way — not a disk-space
problem and not a `MAX_DOCS` problem.

## Options considered

| Option | What it buys | What it threatens |
| --- | --- | --- |
| **A. Do nothing** | Zero code; the layout stays the single source of truth and every `AGENTS.md` claim stays true | Linear-in-age growth of id-addressed reads (`src/store.rs:1863-1874`), lifetime scans (`src/store.rs:1930-1939`), the index map (`src/store.rs:605`) and backup file count (`src/backup.rs:61-93`) |
| **B. Archive by age to a cold tree** (`archive/entries/<YYYY-MM>/…`) with a manifest | The hot tree stops growing; cold data stays human-readable and inside the same backup | (1) the #336 guard reads the `AGENTS.md` "Storage layout" bullet (`tests/contract/store.rs:176-189`, `src/store.rs:252-258`), so the tree must be added to `DOC_COLLECTIONS` (`src/store.rs:265-279`) *and* the bullet in one change or the build fails; (2) a move outside the write lock (`src/store.rs:352-377`) races a concurrent `put_entry`; (3) reads must merge hot+cold or explicitly refuse — a silent split changes lifetime scans, and budgets are lifetime totals (`src/budgets.rs:125-127`) |
| **C. Export then delete** (a backup plus a tombstone) | Shrinks the live tree hardest; the export is the record | The filesystem stops being the store for the exported ids: `get_entry` can no longer address them (`src/store.rs:1863-1874`); the tombstone duplicates the audit trail (#52) without its guarantees; a settled invoice's rate/hours evidence leaves the live tree even though the lock had released it (`src/lock.rs:151-156`) |
| **D. Compact a month into one document** (`entries/<YYYY-MM>.json`) | Fewest files; day folders collapse | Breaks the one-document-per-`Uuid`, date-sharded path builders the store derives everything from (`src/store.rs:1-9`, `1868-1870`); the #335 index assumes day keys (`src/store.rs:605`, `695-708`); one row edit becomes a read-modify-write of a whole month, and tmp+rename protects one document, not a merge of many — blast radius grows from one entry to a month |
| **E. Queryable detached partition** (`entries-cold/`, served by the API) | Cold data stays queryable and hot reads stay fast | The most code for the least product: a second root, a merge layer over `get_entry`/`list_range`/`scan_all_entries`, a second `MAX_DOCS` accounting and a second per-user index — the highest fork risk in a file-based product with no database |

The money/hours invariants are safe in every option provided a move copies bytes
without decoding and re-encoding (`u64` minor units, `u32` hundredths — ADR-001 §2)
and uses the store's tmp+rename discipline rather than `mv`.

## Decision

**Defer, with a trigger.** No archival job is warranted today: the tree is small at
this product's design point (a small team billing its own tracked work), and every
option that shrinks it (B–E) touches the single-writer invariant, the read paths or
the #336 guard — risk now for no measured payoff. This is a decision *not to act*,
recorded with the threshold that would overturn it and the work that must exist first.

**Trigger** — measured on the live data dir with stock tools; any one is sufficient:

```sh
du -sh "$TUCANO_DATA_DIR/entries"        # > 1 GiB
ls "$TUCANO_DATA_DIR/entries" | wc -l    # > 3 650 day folders (~10 years)
```

or, observed at runtime, a `list_all_entries`-backed request (`src/store.rs:1921-1928`,
the admin budget report) or a `--backup` run exceeding ~60 s. Each means the
count/time cost has outgrown the design point; below them an archival job is churn
with a correctness risk and no payoff.

**What an operator does when it fires:** run the maintenance primitive below with
`--dry-run`, read the report (days, documents, bytes, and the entries *skipped* for
being locked or invoice-referenced), take a `--backup`, then run it for real and keep
the manifest with that backup.

**What is archived, and how integrity is proven.** A whole day folder's `<id>.json`
documents — the same bytes moved, never decoded and re-encoded — plus that day's slices
in `entries/.idx.users.json` (`src/store.rs:605`), which must leave the hot index
because it may only name days that are on disk. A per-run manifest of sha256s, the
shape `src/backup.rs:104-117` already uses to prove a #113 archived PDF survived, is
what a restore verifies against.

**Preconditions — must be built before the trigger is actionable:**

1. (#349) A store-owned `archive_entries_before(cutoff, dry_run)` primitive that (a) takes
   the single write lock for the whole pass (`src/store.rs:352-377`), (b) **refuses**
   any day holding an entry that is locked (`CombinedLocks`, `src/lock.rs:95-149`) or
   on any non-draft invoice (`domain::already_invoiced`, `src/domain.rs:1463-1466`),
   (c) moves whole day folders with tmp+rename rather than `mv`, (d) writes a per-run
   manifest of sha256s in the shape `src/backup.rs:104-117` already uses, and (e) drops
   the relocated days' keys from `entries/.idx.users.json` (`src/store.rs:605`) so the
   index shrinks with the documents.
2. (#350) A read story for the cold tree: `get_entry`/`list_range` merge it, or the refusal
   is explicit and documented — and the lifetime scans behind budgets
   (`src/budgets.rs:125-127`) must still see cold data, or the ADR must state that
   budget totals become hot-only.
3. The #336 guard kept honest: the archive tree added to `DOC_COLLECTIONS`
   (`src/store.rs:265-279`) and to the `AGENTS.md` bullet (`AGENTS.md:52-67`) in the
   same change.

**Out of scope:** a schedule, a default cutoff and any automatic deletion. The policy
is operator-run and threshold-triggered — the `--backup`/`--restore` shape already in
`src/backup.rs:1-11` — not a background job.

**This decision changes no documented behaviour**, so `AGENTS.md`'s storage-layout
bullet (`AGENTS.md:52-67`) and the #336 guard's `DOC_COLLECTIONS` list
(`src/store.rs:265-279`) are untouched by this ADR. When option B is implemented, the
guard requires both to change in the same commit.

## Consequences

**Harder:**

- Restores become two-step (the archive is a second tree to place back), and the
  sha256 manifest habit (`src/backup.rs:104-117`) is what proves a move was lossless.
- An invoice referencing an archived day stays legal evidence, so the safety check
  consults invoices forever rather than once.
- `audit.log` (#52) records the mutation but not the file move; the per-run manifest
  is what makes a relocation auditable.
- `entries/.idx.users.json` must be rewritten in the same pass or the days it still
  names force a rescan on every member read (`src/store.rs:703-706`) — #335's own flag.
- The #336 guard turns "add a cold tree" into a two-file change (code list +
  `AGENTS.md`), deliberately.

**Unchanged:**

- The date-sharded layout, one document per `Uuid`, atomic temp+rename writes and
  `MAX_DOCS`: an archived day is the same JSON in a different folder.
- Money and hours stay minor units and hundredths — a byte-copying move never parses them.
- The lock seam and `domain::already_invoiced` remain the sole authority on what may
  move; no retention code may invent its own predicate.
- Normal queries were already bounded to 400 days (`src/store.rs:29`).
- The GUI reads no storage and needs no change.

## Follow-up work

Raised with this ADR; neither is implemented here.

1. **#349 — Store-owned archival primitive:** `archive_entries_before` with dry-run,
   the lock/invoice refusal predicate, a per-run sha256 manifest and the index shrink.
   This is the precondition that makes the trigger actionable.
2. **#350 — Cold-tree read story:** merge cold days into `get_entry`/`list_range`/
   `scan_all_entries`, or an explicit, documented refusal. Without it a relocation
   silently changes what queries and budget totals see.
