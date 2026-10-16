# Ubuntu/Vanilla rollout — page-by-page migration plan

Goal: standardise the whole GUI on Ubuntu/Vanilla components **without any
big-bang layout change**. Each page is migrated, reviewed live at its own URL,
and signed off before the next one starts. This doc is the tracker;
[`ubuntu-design.md`](ubuntu-design.md) holds the design rationale and the
component backlog.

## Prerequisites (done)

- **Stage 1 (#191)** — Vanilla bundle extended with `p-table`, `p-card`,
  `p-chip`, `p-badge`, `p-notification`, `p-modal`, `p-tabs` (additive).
- **Cards, first pair (#229)** — first-admin sign-in and the setup wizard are
  `p-card` twins.
- **Page routing** — the JSON API moved under `/api`; every view now has a
  stable, case-insensitive URL (`spa_page` in `src/lib.rs`, `TAB_ROUTES` in
  `web/app.js`). This is what makes per-page review-and-sign-off practical.
- **Auth + form controls (#232 increment 1)** — the vendor bundle now carries the
  official form family (`vf-b-forms`/`vf-p-links`); the hand-rolled control
  styles are retired and the auth/wizard/entry/user/customer forms use the
  explicit `p-button--positive`/`p-button` classes. Tables, chips, dialogs and
  tablists follow in their own #232 increments.
- **Reference tables (#232 increment 2)** — the customer/project/task/expense/report
  registers are `p-card` shells (their `p-card__header` is the #230 heading slot), and
  the dense registers use `p-table--mobile-card` with renderer-supplied `data-heading`
  labels. The vendor bundle is unchanged (tables/cards shipped in stage 1), so the
  `manifest.json` `cssSha256` stays `c6b404f72a9fb86c5a1425af9f26b27870edf2e9da74c640dbb149390d936e05`.
- **Chips, badges + notifications (#232 increment 3)** — every status pill is the
  official `p-chip` / `p-chip__value` (one shared `statusChip` builder in
  `web/js/dom.js`; `p-status-label` is deprecated upstream and folded into the
  chip), the editable expense-category chips use `p-chip__dismiss`, and the
  notification list is canonical `p-notification` markup. The bespoke `.badge` /
  `.chip` / `.notif` skins are retired. Chips, badges and notifications shipped in
  stage 1, so the vendor bundle is unchanged again and `cssSha256` is still
  `c6b404f72a9fb86c5a1425af9f26b27870edf2e9da74c640dbb149390d936e05`.
- **Dialogs (#232 increment 4)** — every popup is the official Vanilla `p-modal`
  overlay on the native `<dialog>` element (top layer/`::backdrop`/inert/focus
  trap kept from #102) wrapping a `p-modal__dialog` box with a
  `p-modal__header`/`p-modal__title` and a `p-modal__footer`. The first-run
  wizard keeps its #229 `p-card` frame and only its dialog becomes `p-modal`. The
  nine bespoke `#…-dialog` skins, their aubergine `::backdrop` tint and the
  generic `dialog`/`dialog h2` overrides are retired; only per-dialog `max-width`s
  stay app-owned. A shared `initDialogFocusReturn()` returns focus to the invoker
  on close. The component family shipped in stage 1, so the vendor bundle is
  unchanged and `cssSha256` is still
  `c6b404f72a9fb86c5a1425af9f26b27870edf2e9da74c640dbb149390d936e05`.
- **Tablists (#232 increment 5)** — the sidebar rail, the Timesheets
  `Day|Week|Calendar` bar and the Settings bar are the official `p-tabs__list` /
  `p-tabs__item` / `p-tabs__link` pattern (ARIA tab roles, roving `tabindex`,
  `aria-controls`/`tabpanel` wiring and the #258 Up/Down vs Left/Right key
  contract all unchanged); the bespoke `.tabs [role="tab"]` and
  `.seg [role="tab"]` skins are retired. Vanilla's `.p-tabs` container is not
  used — it boxes list+panels and its `overflow: hidden` would clip focus rings,
  while here the panels live in `<main>`. The component family shipped in stage 1,
  so the vendor bundle is unchanged and `cssSha256` is still
  `c6b404f72a9fb86c5a1425af9f26b27870edf2e9da74c640dbb149390d936e05`. Mapping
  correction: the Invoices Open|All control named below is a `role="group"` of
  `aria-pressed` toggle buttons, not a tablist — it keeps the app segmented skin.

- **CSS cleanup (#232 increment 6)** — the stylesheet is reduced to the layout
  shell + the app-owned grids and rhythm. The audit confirms no retired
  `.badge`/`.chip`/`.notif`/dialog/tab skin survives (increments 1–5 deleted
  them) and removes the eight rules the cascade had already shadowed (see the
  increment-6 note in [`ubuntu-design.md`](ubuntu-design.md)), together with the
  deferred `#dlg-input { color: var(--text) }` fix (`--fg`) and a new
  `gui-test/css-tokens.test.mjs` guard that every `var(--…)` resolves in both
  themes. No component family changed → `npm run build:ubuntu` is a **no-op**
  and `cssSha256` is still
  `c6b404f72a9fb86c5a1425af9f26b27870edf2e9da74c640dbb149390d936e05`.

- **Remaining registers (#232 phase 2a)** — the still-bespoke registers moved onto
  the same pattern: the user (`/settings/users`), approval (`/approvals`),
  credential (`/settings/security`), catalog (`/settings/catalog`) and the
  advanced-billing recurring/retainer (`/invoices`) registers are `p-card` shells
  using `p-table--mobile-card` with renderer-supplied `data-heading` labels, while
  the runtime-config and template-variable summaries keep a plain official table
  inside their card. The three bespoke register rules the conversion replaced
  (`#config-table`, `#user-table`, `.advanced-billing-section table`) are deleted,
  so `styles.css` drops from 701 lines / 389 rules to 698 / 386. The `#report-table`
  shell had already landed in increment 2. The component family is unchanged, so
  `npm run build:ubuntu` is a **no-op** and `cssSha256` stays
  `c6b404f72a9fb86c5a1425af9f26b27870edf2e9da74c640dbb149390d936e05`.

- **Form fields, hints and errors (#232 phase 2b)** — every `.field` row moved onto
  the official `p-form__group` + `p-form__label` + `p-form__control` vocabulary
  (113 groups / 109 label-control pairs, in `web/index.html` and the builders in
  `web/js`), the 27 panel/dialog hints became `p-form-help-text`, and all 22 error
  paragraphs became a `p-form-validation__message` inside a
  `p-form-validation is-error` group — so the bespoke `.field`, `.hint` and
  `.form-error` rules leave the sheet. Three things stay app-owned and commented:
  the constant `.65rem` row rhythm (Vanilla spaces stacked groups only inside
  `.p-form--stacked`, and only at >= 1036px), the 1rem tick box (the base rule
  that forces `width:100%` does not cover checkboxes) and the grouped-fieldset
  frame (the base `fieldset` is a tinted, clipping card). One scoped override,
  `.p-form-help-text { margin: revert }`, keeps that class's own pull-up
  (`margin-top: -.5rem`) off this app's panel/dialog copy, which follows a
  heading rather than an input, and the error ink moves from `--danger` to
  Vanilla's negative token (AA in both themes) — **both flagged for the
  maintainer's visual sign-off**. The increment also threaded
  `data-icon`/`data-positive` through every button-creation path, so
  `decorateUbuntuButton`'s label/type/id fallbacks are now redundant (deleted in
  phase 2e). `styles.css` moves from 698 lines / 386 rules to 715 / 383 (the same
  `grep -c '{'` method as the earlier counts); the component family is unchanged,
  so `npm run build:ubuntu` is a **no-op** and `cssSha256` stays
  `c6b404f72a9fb86c5a1425af9f26b27870edf2e9da74c640dbb149390d936e05`.

- **Bespoke cards retired (#232 phase 2c)** — the last `.card` skin leaves the
  sheet: all 15 former `.card` sites (the invoice wizard/editor/preview regions,
  the retainer ledger, the template preview and every settings/advanced-billing
  form) are the official `p-card`, with the heading-slot panels (wizard, editor,
  preview, retainer ledger, template preview) using `p-card__header`. The Day
  entry form **drops** the class rather than gaining `p-card` — it is the body of
  a `p-modal__dialog`, which already supplies the frame — and the four bespoke
  rules the conversion replaced (`.card`, `.settings-panel > .card`,
  `#entry-dialog form.card`, `.advanced-billing-section .card`) are deleted.
  `p-card__footer` is **not** used: Vanilla 4.59 ships no such class. The
  increment also fixes a defect the phase-2b review surfaced —
  `#expense-dialog input { width: 100% }` (1,0,1) out-specified the 1rem tick-box
  sizing (0,2,1) by id count, so `#expense-billable` painted 100% wide; the width
  rule is scoped to `input:not([type="checkbox"]):not([type="radio"])` and pinned
  by `gui-test/input-width.test.mjs` and the browser gate. `styles.css` moves
  from 715 lines / 383 rules to 724 / 379 (375 comment-stripped); the component
  family is unchanged, so `npm run build:ubuntu` is a **no-op** and `cssSha256`
  stays `c6b404f72a9fb86c5a1425af9f26b27870edf2e9da74c640dbb149390d936e05`.

- **Attribute-only button decoration (#232 phase 2e)** — `decorateUbuntuButton`
  no longer infers a button's icon or colour from its label, type or id, and no
  longer strips a leading `+` from a label: decoration is a pure function of the
  explicit `data-icon` / `data-positive` attributes that every creation site
  (the 12 builders and the shared `rowAction`) already states. The deleted
  fallback only ever affected dynamic labels built from user data (a customer
  named "Copy Center" used to acquire a copy glyph), so nothing lost a glyph it
  should keep. `gui-test/forms.test.mjs` asserts the attribute contract directly
  and the real-browser gate asserts it on every routed view (107 embedded / 113
  dev checks).

- **Documentation truth + final CSS dedupe (#232 phase 2f)** — the
  bundle-as-authority ground rule above was added as the phantom-class family
  (`p-field` x3, plus the already-corrected `p-table--sortable` /
  `p-modal__body` / `p-card__footer`) was swept out; the base-element
  contradiction with the issue's ground rule 1 was resolved in
  [`ubuntu-design.md`](ubuntu-design.md) (the vendored `vf-b-*` base layers
  legitimately emit global element rules; the app adds none of its own); the
  deliberate non-migrations were recorded as decisions; and two provably
  redundant `table` declarations plus the verbatim `#week-table.grid` duplicate
  were deleted. `styles.css` moves from 726 lines / 377 rules (373
  comment-stripped) to 725 / 376 (372).

**Umbrella status.** Increments 1–6 and phases 2a–2e are all merged, so the
component-substitution scope, the shared form vocabulary and the attribute-only
button decorator are complete. #232 stays **open** (*Part of*) for exactly one
remaining item: the page map above is still ⬜ — every row needs the maintainer's
manual light/dark + keyboard/AT review of ground rules 4–5, including the
Invoices sortable register — and that sign-off is what a later ticket needs
before #232 can be closed.

## Ground rules

0. **The vendored bundle's compiled CSS is the authority for class names.**
   `web/vendor/ubuntu/components.css` (the pinned Vanilla 4.59 build) settles
   whether a `p-*` class exists; this plan's prose is a guide and **must be
   verified against that bundle before implementing**. Earlier revisions named
   four classes that never existed in 4.59 — `p-table--sortable`,
   `p-modal__body`, `p-card__footer` and `p-field` — and each cost wasted
   effort; the bundle settles the question, not the prose. (The `vf-b-*` /
   `vf-p-*` names below are **Sass layer/mixin** names, not emitted class
   selectors — the compiled bundle carries their output, not their names.)
1. **One PR per page view**, merged into `main`; review happens at the
   page's URL before the next page starts.
2. **Layout stays**: component substitution and styling only — no rearranging
   panels, moving controls or rewording flows unless a ticket says so.
3. **Element ids are frozen** (`gui.mjs`, `a11y.test.mjs`, `users.test.mjs`
   etc. drive them). New markup wraps or re-classes, never renames.
4. Each PR ships: the page's Vanilla classes + any shared adapter it needs,
   jsdom/contrast updates, `docs/ubuntu-design.md` note, and a green gate:
   `cargo test --all-targets` + `node --test *.test.mjs` + `node contrast.mjs`
   + `node gui.mjs` (plain server, as in CI).
5. Both themes are reviewed per page (`.is-dark`/`is-light` tokens) and focus
   rings must stay visible inside `p-card`/`p-modal` overflow.
6. The GUI is pure presentation: no route or contract change is ever needed
   for these PRs — anything touching `src/` or `openapi.json` is a separate
   ticket.

## Page map (review URLs)

| # | Page | URL to review | Status |
|---|------|---------------|--------|
| 0 | Sign-in / first-admin + setup wizard | `/` (pre-auth) | ✅ #229 |
| 1 | Timesheets — Day | `/timesheets/day` | ⬜ |
| 2 | Timesheets — Week | `/timesheets/week` | ⬜ |
| 3 | Timesheets — Calendar | `/timesheets/calendar` | ⬜ |
| 4 | Customers | `/customers` | ⬜ |
| 5 | Projects | `/projects` | ⬜ |
| 6 | Tasks | `/tasks` | ⬜ |
| 7 | Invoices | `/invoices` | ⬜ |
| 8 | Expenses | `/expenses` | ⬜ |
| 9 | Approvals | `/approvals` | ⬜ |
| 10 | Reports | `/reports` | ⬜ |
| 11 | Settings — Security & runtime | `/settings/security` | ⬜ |
| 12 | Settings — Users | `/settings/users` | ⬜ |
| 13 | Settings — Invoice documents | `/settings/invoices` | ⬜ |
| 14 | Settings — Products & services | `/settings/catalog` | ⬜ |
| 15 | Settings — Expense categories | `/settings/expenses` | ⬜ |

Cross-navigation (row "Projects" buttons, wizard finish, calendar → day) is
expected to keep working through the tabs' click handlers; a follow-up ticket
may turn those into real links once every page is migrated.

**#232 phase 2a progress (registers only).** The register of each view below is
now on the official pattern, but every row stays ⬜ because a page is not
"migrated" until its forms/dialogs and the per-page light/dark + keyboard/AT
review land too: the user (`/settings/users`), approval (`/approvals`), credential
(`/settings/security`), catalog (`/settings/catalog`) and the advanced-billing
recurring/retainer (`/invoices`) registers are `p-card` + `p-table--mobile-card`
shells, and the runtime-config (`/settings/security`) and template-variable
(`/settings/invoices`) summaries are plain official tables in a `p-card`. The
Day/Week/Calendar grids and the invoice line editor are deliberately untouched;
the Invoices sortable register (row 7) is now **phase 2d** — the real `aria-sort`
+ `.p-table__sort-button` pattern (there is no `.p-table--sortable` class in
Vanilla 4.59).

**#232 phase 2b progress (forms).** Every page's stacked form rows, hints and
error messages are on the official vocabulary now (see the phase-2b bullet
above), but the rows stay ⬜ for the same reason: the per-page light/dark +
keyboard/AT review has not happened yet. Deliberately still app-owned: the
horizontal filter/tool bars (`.bar`, `.filter-field`), the `#dlg-input` prompt
skin, the `.theme-switch` fieldset and the `.cols-pop` column picker.

**#232 phase 2c progress (cards).** Every bespoke `.card` is retired in favour of
the official `p-card` (see the phase-2c bullet above), so the last card skin left
the sheet; but the rows stay ⬜ for the same reason as 2a/2b — the per-page
light/dark + keyboard/AT review has not happened yet — and the Invoices sortable
register has since landed in phase 2d (below). Deliberately still app-owned: the
horizontal filter/tool bars (`.bar`, `.filter-field`), the `#dlg-input` prompt
skin, the `.theme-switch` fieldset and the `.cols-pop` column picker.

**#232 phase 2d progress (the sortable register).** The Invoices register is on
Vanilla 4.59's real sortable pattern — `aria-sort` on the `<th>` plus a
`.p-table__sort-button` — because there is **no `.p-table--sortable` class** in
4.59 (the pattern is the `vf-p-table-sortable` mixin; the earlier plan prose that
named a `.p-table--sortable` class was wrong and is corrected here). The sortable
columns, their order, the ascending/descending toggle, the default (`Number`
ascending) and the `data-col` keys are unchanged: the active column carries
`ascending`/`descending`, every other sortable column is `none`, and the static
markup now reflects that (the bespoke `.sort-th` skin left every heading
un-annotated until the first click — a screen-reader gap this closes). The row
content, status pill, empty state and the totals/summary behaviour are untouched.
The bespoke `.sort-th` rules are deleted; `styles.css` moves from 724 lines / 379
rules to 726 / 377 (373 comment-stripped). Row 7 still needs the
per-page light/dark + keyboard/AT review. Guards: `gui-test/sortable.test.mjs`
(static contract + `dashFilter` semantics), the `gui.mjs` live sort assertions
and the real-browser gate's new sortable audit.

## Order and per-page scope

The sequence deliberately front-loads the shared adapters (Day carries the
first `p-table` register and the entry form; Invoices carries the most
patterns). Each page lists what "migrated" means — nothing layout-level.

1. **Timesheets — Day.** `.day-bar` already uses `p-card`; the entry form is on
   the official `p-form__group` rhythm (phase 2b), the register is `p-table`,
   statuses are `p-chip` (the shipped `p-badge` is a numeric counter, not a text
   status — see `ubuntu-design.md`), and the copy-forward bar is the shared card
   adapter. Deliver: the `p-table` adapter used by every later register.
2. **Timesheets — Week.** Grid keeps its app-owned geometry (explicitly out of
   scope per `ubuntu-design.md`); the surrounding controls remain
   (the Day|Week|Calendar strip is already the official `p-tabs__list` since
   increment 5; week totals footer affordances still to do).
3. **Timesheets — Calendar.** Month navigation cluster + legend onto shared
   button/card adapters; grid cells stay app-owned.
4. **Customers.** Register onto the Day `p-table` adapter; create/edit popup
   onto the `p-modal` skin; contact rows onto form rhythm.
5. **Projects.** Same adapters; the scope label + filter row stay the app-owned
   horizontal `.bar` / `.filter-field` rows (a decided non-migration: Vanilla's
   `p-form--inline` only lays out at >= 1036px and is not this compact tool row).
6. **Tasks.** Same; hierarchy filters onto the shared filter-field pattern.
7. **Invoices.** Biggest page: register onto the real sortable pattern
   (`aria-sort` + `.p-table__sort-button`; there is **no** `p-table--sortable`
   class in Vanilla 4.59) — landed in phase 2d — preview panel
   + tracked-work wizard dialogs onto the modal/card adapters, payment dialogs
   included. The Open|All control is **not** a tablist (it is a `role="group"` of
   `aria-pressed` toggle buttons), so it keeps the app's segmented `.seg` /
   `.seg-btn` skin rather than moving onto `p-tabs`.
8. **Expenses.** Register + quick-add category popup reuse earlier adapters.
9. **Approvals.** Submission table + decision dialogs reuse earlier adapters.
10. **Reports.** Query bar stays the app-owned horizontal `.bar` row (decided
    non-migration; see item 5), result table onto `p-table`, CSV link as
    `p-button--link` pattern.
11. **Settings — Security & runtime.** Config table + theme switch onto
    form/button adapters.
12. **Settings — Users.** User register + create/reset-password dialogs.
13. **Settings — Invoice documents.** Template editor + preview onto card
    adapters.
14. **Settings — Products & services.** Item catalog table + dialog.
15. **Settings — Expense categories.** Table + dialog; any further
    `web/styles.css` shrink ships as its own PR when it exceeds a page's worth
    of churn. The `.card` skin this item used to name was retired early in
    **phase 2c**; the `.bar` skin is **not** retired — it is a decided
    non-migration (see `ubuntu-design.md`).

## Per-page review workflow

For page *N*: implement → open PR → **hold merge** until you review
`http://<host>:8080/<url>` (or the PR's dev server) in **both themes**, check
keyboard/AT basics, then approve → merge → tick the table above → next page.
Referencing a page in chat is just its URL, e.g. *"review
`/timesheets/week`"* — it deep-links from a fresh session after login.

## Tracking

Suggested: one GitHub issue per row (title `Vanilla rollout: <page>`, label
`ui`, blocked-by the routing foundation issue), mirroring the table above;
this doc stays the index.
