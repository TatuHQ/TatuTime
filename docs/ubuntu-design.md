# Ubuntu and Vanilla Design Review (#181)

## Findings

The existing UI used Ubuntu colour names but not the Ubuntu typeface. Small
secondary text, rounded controls, inconsistent glyph/emoji icons and similarly
prominent actions made it feel less like a Canonical operational application.
Forms and dense tables needed stronger focus and boundary affordances, and
larger typography exposed narrow-screen overflow in some registers.

## Decision

Use real, selected components from Canonical's Vanilla Framework 4.59.0:
button variants, theme variables, official icons and Ubuntu variable font faces.
Keep the application-owned navigation, native dialogs and editable timesheet
grid. A complete Vanilla reset would unnecessarily change specialised layouts
and require broader workflow migration; no React rewrite is needed.

Generated CSS, font files, licences, asset hashes and corresponding framework
source are in `web/vendor/ubuntu/`. All runtime assets are served by the binary;
no CDN or Node process is needed to build/run the Rust application. Font display
uses swap. The font retains its full width axis; numbers use tabular alignment
where values are compared. The existing colour-mode preference drives both
application tokens and Vanilla's `is-light`/`is-dark` component themes.

To regenerate assets, run `npm ci` in `gui-test`, then `npm run build:ubuntu`.
Build tooling is pinned in its package lock. Sass compiles only the mixins in
`ubuntu-components.scss`; the generator parses CSS URLs, downloads the official
fonts, rewrites them to local paths and records hashes. It includes the original
Vanilla npm source archive, LGPL notice and accompanying GPLv3 text; Ubuntu fonts retain their font
licence. Local application adapters are not Canonical branding or endorsement.

## Staged whole-GUI adoption (#191)

`#183` reviewed the remaining bespoke controls against vanillaframework.io/docs
and design.ubuntu.com. Since #181 the generated bundle carried icons and buttons
only; **#191 stage 1** extended it with the official `p-table` (base + sortable +
mobile-card), `p-card`, `p-chip`, `p-badge`, `p-notification`, `p-modal` and
`p-tabs` patterns. They are compiled in but initially **additive**: no app
template uses them yet, so current screens render unchanged (verified — the
contrast and jsdom suites pass on the rebuilt bundle).

**#232 stage 2** migrates the app templates onto those official patterns, one
view family per PR. **Increment 1 (auth + form controls)** extends the bundle
with the official form family — `vf-b-forms` (the label/input/select/textarea
rhythm plus the `p-form`/`p-form-validation`/`p-form-help-text` patterns) and
`vf-p-links` (`p-link--soft`) — retires the hand-rolled control styles from
`styles.css`, and switches the sign-in window, the setup wizard and the
entry/user/customer/project/task forms onto the explicit `p-button--positive`
and neutral `p-button` classes. Tables, chips/badges, dialogs and tablists
follow in their own increments (tracked on #232).

**Increment 2 (reference tables)** wraps the customer/project/task/expense/report
registers in the official `p-card` shell — the `p-card__header` is the #230 heading
slot (creation action/filters), so its layout is unchanged — and the dense registers
adopt the `p-table--mobile-card` pattern, whose `data-heading` labels the renderers
now emit. The report register keeps the plain official table because its totals
footer is not a mobile-card shape. No new component family was needed (tables and
cards shipped in stage 1), so `npm run build:ubuntu` is a **no-op** and the
`manifest.json` `cssSha256` is unchanged.

**Increment 3 (chips / badges / notifications)** renders every status pill as the
official Vanilla chip — `p-chip` + `p-chip__value`, with `is-inline` so the pill
sits on a table / note baseline — through one shared `statusChip` builder in
`web/js/dom.js`, retiring the bespoke `.badge` / `.badge.on` rules. Vanilla 4.59
**deprecates** `p-status-label` ("merged into the Chip component",
canonical/vanilla-framework#5535), which is why the chip — not the label pattern
this backlog originally named — is the successor; the app's pre-existing binary
emphasis (`.badge.on`) maps to `p-chip--positive` vs the neutral chip, a
deliberate colour-of-meaning change from the old aubergine pill. The editable
expense-category chips become `p-chip` (`__value` + the official `__dismiss`
close control); the notification list renders the canonical
`p-notification--<severity>` + `p-notification__content` structure, severity wired
to the backend `kind` (every kind emitted today is informational), while the
polite `#live-region` and field-level `role="alert"` announcement surfaces are
unchanged. No new component family was needed (chips, badges and notifications
shipped in stage 1), so `npm run build:ubuntu` is a **no-op** and the
`manifest.json` `cssSha256` is unchanged.

**Increment 4 (dialogs)** puts every popup on the official Vanilla `p-modal`
pattern. The native `<dialog>` element is **kept** and carries `p-modal` — it
still owns the top layer, `::backdrop`, inert background, focus trap and Escape,
so the #102 security behaviour is unchanged — while the box inside it is the
official `p-modal__dialog` with a `p-modal__header`/`p-modal__title` and a
`p-modal__footer`. Only the UA box the native element adds (border, max sizes) is
neutralised, so the chrome, colours and internal scrolling come from the vendored
classes (`--vf-color-background-overlay` dim, `…-background-default` box,
`…-text-default` ink). The nine bespoke `#…-dialog { … }` skins, their aubergine
`::backdrop` rules and the generic `dialog` / `dialog h2` overrides are retired;
only per-dialog `max-width`s (26–38 rem) stay app-owned, on the `p-modal__dialog`
box. The first-run wizard keeps its #229 `p-card` frame (a twin of the sign-in
overlay) and only its outer dialog adopts `p-modal`. A shared
`initDialogFocusReturn()` in `web/js/dom.js` makes the native dialog's
focus-return contract explicit for **every** popup (a browser restores focus to
the invoker on close; jsdom does not), and each dialog now declares
`aria-modal="true"` beside its `aria-labelledby` title. Vanilla 4.59.0 shipped
`p-modal` in stage 1 and has **no `p-modal__body`** rule — the pattern's body is
the plain content between header and footer, so the vocabulary earlier in this
backlog was corrected — and no new component family was needed, so
`npm run build:ubuntu` is a **no-op** and the `manifest.json` `cssSha256` is
unchanged.

**Increment 5 (tablists)** puts every app tablist on the official `p-tabs`
vocabulary — `p-tabs__list` on the `role="tablist"` element, each tab wrapped in
a `p-tabs__item` and the button carrying `p-tabs__link` — across the sidebar rail,
the Timesheets `Day|Week|Calendar` bar and the Settings bar. The ARIA tab pattern
and the #258 roving-tabindex contract are unchanged: `role="tab"`,
`aria-selected`, exactly one tab in the tab order, `aria-controls` → panel
`role="tabpanel"` + `aria-labelledby`, and activation still drives the same view
switch and route change (the vertical rail navigates Up/Down, the horizontal bars
Left/Right, both Home/End). Vanilla's `.p-tabs` container is deliberately **not**
used: it exists to box the list *together with its panels* and carries
`overflow: hidden`, so in this single-shell SPA — where the panels live in
`<main>` — it would buy nothing and clip the focus rings. The bespoke
`.tabs [role="tab"]` rail skin, the `.seg [role="tab"]` segment skin and the
duplicated icon margin are retired; the rail keeps the app ink (`--fg`, selected
`--brand-deep` on `--accent-soft`, both pinned in `gui-test/contrast.mjs`) so it
matches the wizard entry beside it, while the two strips take Vanilla's own
colours and bottom highlight bar (whose selected indicator is painted in the tab
text colour — so the active tab can never be *lower* contrast than the inactive
one, the classic tab gap). What stays app-owned is geometry only: the rail's
vertical axis, the strips' heading-slot width, and the #145 uniform segment
widths. Recorded correction to earlier backlog prose: the plan listed the
Invoices Open|All control as a tablist to move onto `p-tabs`, but it is a
`role="group"` of `aria-pressed` toggle buttons, so it keeps the `.seg` /
`.seg-btn` segmented-control skin. `vf-p-tabs` shipped in stage 1, so

**Increment 6 (cleanup)** — the last planned increment audits `styles.css` for
the rules the earlier increments left dead and shrinks it to the layout shell
plus the app-owned grids and rhythm. No `.badge` / `.chip` / `.notif` / dialog /
tab skin remains: increments 1–5 already deleted that vocabulary, so the audit
instead proves the eight surviving rules whose outcome the cascade had already
decided — a dependency-free shadowing check (a later rule with the identical
selector re-declares every property the earlier one set; the redundant inverse
with equal values; and the specificity-scoped duplicates) flags the
`.nav-label`/`.hint`/`.foot` font-size base, the `.card` `box-shadow` superseded
by the card skin, the `#entry-dialog .p-modal__dialog` width duplicated between
the dialog list and its #145 section, the second `.inv-overview` `align-items`,
the overridden `#inv-chart` height, the `#week-table` `white-space` and
`row-total` repeats, and the duplicate `.day-bar.p-card` column track — all
deleted. The `#dlg-input` colour defect increment 4 deferred is fixed
(`var(--text)` → `var(--fg)`) and a new `gui-test/css-tokens.test.mjs` guard
fails the build if any `var(--…)` in `styles.css` is not defined in **both**
themes (verified red against the pre-fix sheet). The component set is unchanged,
so `npm run build:ubuntu` is a **no-op** and `cssSha256` is unchanged.

**Phase 2a (remaining registers, #232 follow-up)** wraps the last bespoke
registers in the official `p-card` shell: the user, approval, credential, catalog
and advanced-billing recurring/retainer registers adopt `p-table--mobile-card`
(their renderers now emit `data-heading` labels), while the runtime-configuration
and template-variable summaries keep a plain official table inside the card (a
key/value + reference shape is not a mobile-card list). The `#user-table`
`min-width`, the `#config-table` `display:block`/`overflow-x` hack and the
`.advanced-billing-section table` `display:block`/`overflow-x` rule the conversion
replaced are deleted (`styles.css` 701/389 → 698/386), and a new
`gui-test/registers.test.mjs` guard pins the classes, the p-card ancestry, the
frozen column order and the rendered row counts. The Day/Week/Calendar grids, the
invoice line editor and the Invoices `p-table--sortable` register are deliberately
left to their own increments. The component set is unchanged, so
`npm run build:ubuntu` is a **no-op** and `cssSha256` is unchanged.

**#232 phase 2b (forms).** The last bespoke form vocabulary is gone: every
`.field` row — 113 wrappers in `web/index.html` plus the builders in `web/js` —
is the official `p-form__group`, with `p-form__label` on its label and
`p-form__control` on its control (109 pairs, all verified in a real browser),
the 27 panel/dialog hints are `p-form-help-text`, and each of the 22 error
paragraphs is a `p-form-validation__message` inside a `p-form-validation
is-error` group. The retired `.field`, `.hint` and `.form-error` skins leave
`styles.css`, which keeps only the rhythm Vanilla does not express: the constant
`.65rem` row rhythm (the base input's own margin sits *inside* the group, and
`.p-form--stacked` only spaces groups at >= 1036px), the 1rem tick box, and the
grouped-fieldset frame (the base `fieldset` rule tints and clips). Two decisions
are flagged for the maintainer's visual sign-off: `.p-form-help-text { margin:
revert }` keeps the class's `margin-top: -.5rem` pull-up out of panel copy that
follows a heading rather than an input, and the error ink moves from the app's
`--danger` to Vanilla's negative token (AA in both themes, `#c7162b` light /
`#d17b85` dark). The base-element audit also found a pre-existing defect —
`#expense-dialog input { width: 100% }` also stretches `#expense-billable` — left
in place for a follow-up decision rather than changed silently here. `styles.css`
is 698 lines / 386 rules → 715 / 383; the component set is unchanged, so
`npm run build:ubuntu` is a **no-op** and `cssSha256` is unchanged. Guards:
`gui-test/forms.test.mjs` (vocabulary, pairing, frozen field contract, button
attribute coverage) and the real-browser gate's new form audit (80 → 86 embedded
/ 86 → 92 dev checks).

**Umbrella #232 status.** Increments 1–6, the phase-2a register sweep and the
phase-2b form sweep are all landed and green, so the component-substitution
scope and the shared form vocabulary are complete; the umbrella stays **open**
(*Part of #232*) only for the page-by-page rollout tracked in
[`ubuntu-migration-plan.md`](ubuntu-migration-plan.md) — rows 1–15 are still ⬜
(the per-page manual light/dark + assistive-technology sign-off in ground rules
4–5, and the Invoices `p-table--sortable` register in its own increment) — and
for deleting the now-redundant `decorateUbuntuButton` fallbacks (phase 2e).
`npm run build:ubuntu` is a **no-op** and `cssSha256` is unchanged.

The rules the plan holds to: no global reset (the editable timesheet grid is
app-owned and must keep its dense cell geometry), the rejected GOV.UK direction
(#175) stays rejected, and automated contrast coverage is never presented as
blanket WCAG conformance.

Backlog — one component family per reviewed PR, behind existing element ids,
each gated by the jsdom + contrast suites and a manual keyboard/AT pass on the
affected screens. **Page-by-page execution order, review URLs and ground rules
moved to [`ubuntu-migration-plan.md`](ubuntu-migration-plan.md)** after the API
moved under `/api` and every view gained a stable path (`spa_page` in
`src/lib.rs` ↔ `TAB_ROUTES` in `web/app.js` — keep them in sync):

0. **Stage 2b (in progress)** — cards: the **first-admin sign-in** and the
   **first-run setup wizard** are now twins built on Vanilla `p-card`
   (`p-card__header` + `p-card__content`) with the ruled header, `.65rem`
   field rhythm and full-width `p-button--positive` action (#229). The
   wizard's native dialog keeps its modal behaviour and backdrop but is left
   chromeless so the inner `p-card` is the single frame, matching the auth
   overlay exactly. Its first-customer field is prefilled with the `Tucano
   Time` example. Element ids are unchanged, so the jsdom + wizard smoke
   suites stay the gate. The remaining `.card` sites follow below.
1. **Stage 2a** — tables: ✅ the customer/project/task/expense registers (inc 2)
   and the submission/user/credential/catalog/recurring/retainer registers
   (phase 2a) are `p-card` shells — the dense registers use `p-table--mobile-card`,
   the config/template-vars summaries a plain official table. Still open: the
   Invoices register gains `p-table--sortable` in its own page increment.
2. **Stage 2b** — cards/chips/badges: ✅ cards (#229, #232 inc 2); chips/badges
   (#232 inc 3) — `.card`/`.chip`/`.badge` are now `p-card`/`p-chip` adapters. The
   old backlog named `p-badge`, but the shipped `p-badge` is a **numeric counter**
   (4ch-wide, overflow hidden), so text statuses are the chip (see the
   increment-3 note); the count badge has no caller in this GUI.
3. **Stage 2c** — notifications and modal dialogs: ✅ both (#232 inc 3
   notifications, inc 4 dialogs). The `.notif` list is canonical
   `p-notification`; every popup is the official `p-modal` overlay +
   `p-modal__dialog`/`__header`/`__title`/`__footer` internals, keeping the
   native `<dialog>` element (#102 security behaviour unchanged) and its p-card
   twin frame for the wizard.
4. **Stage 3** — ✅ sidebar tabs and segment controls onto the official
   `p-tabs` vocabulary (#232 inc 5; the `.p-tabs` container is intentionally not
   used — see the increment note above). Still open: delete the label/type/id
   fallbacks in `decorateUbuntuButton` (phase 2e) — every shell and builder
   creation site now states `data-icon`/`data-positive`, so the fallbacks are
   redundant (attribute support shipped with #191).
5. **Stage 2d — forms**: ✅ fields, hints and error messages (#232 phase 2b) —
   `p-form__group`/`p-form__label`/`p-form__control` rows, `p-form-help-text`
   hints and `p-form-validation__message` errors replace `.field`, `.hint` and
   `.form-error`; the app-owned rhythm, tick box and fieldset frame stay (see
   the phase-2b note above, which also lists the two sign-off flags). Still
   open: the horizontal filter/tool bars, the `#dlg-input` prompt skin and the
   `.theme-switch`/`.cols-pop` controls, which are not stacked form rows.

## Applied Changes

- Ubuntu variable body font, 16px text, restrained heading weights and no
  negative letter spacing or viewport-scaled type.
- Official neutral/positive buttons; Ubuntu orange remains a navigation accent.
- Official navigation, previous/next, play/stop, edit/delete, copy and lock/note
  icons with decorative `aria-hidden` markup and preserved control labels.
- Stronger field boundaries, stable control sizes, clear selected states,
  reduced-motion handling and contained horizontal scrolling for dense tables.
- Existing labelled fields, keyboard tabs, aria-live announcements, native
  dialog focus/escape behavior and exact money calculations remain intact.
- **#191 accessibility fixes**: the setup-wizard button moved out of the
  `role="tablist"` (non-tab children violate required-owned-children); the
  invoice chart's per-month summary moved from a `role="img"` subtree (never
  announced) into the accessible name; contact-editor and invoice-line inputs
  gained real accessible names instead of placeholder-only; faint interactive
  affordances lifted to `opacity .7`; icon/positive styling now honours
  `data-icon`/`data-positive` at creation sites, with the old label/id matching
  kept only as a fallback.

## Accessibility Verification Scope

Automated checks cover palette and real component contrast, local font/asset
integrity, accessible icon names, persistence of themes, dialog security and the
full GUI workflows. Browser checks cover actual font loading, both themes,
desktop/mobile framing, keyboard focus and reflow. This is not a blanket WCAG
conformance claim: full assistive-technology testing and a content-wide error
association audit remain separate work. Dense data grids may scroll within
their container rather than compressing their columns.

## References

- https://design.ubuntu.com/
- https://design.ubuntu.com/accessibility
- https://vanillaframework.io/docs/customising-vanilla
- https://vanillaframework.io/docs/base/typography
- https://vanillaframework.io/docs/patterns/buttons
- https://vanillaframework.io/docs/patterns/icons/accessibility