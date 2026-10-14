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
1. **Stage 2a** — tables: the customer/project/task/expense/submission/user
   registers onto `p-table` (invoice register gains `p-table--sortable`;
   dense registers gain the mobile-card pattern for narrow screens).
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
4. **Stage 3** — sidebar tabs and segment controls onto `p-tabs`; retire the
   label-matching fallback in `decorateUbuntuButton` once every creation site
   passes `data-icon`/`data-positive` (attribute support shipped with #191).

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