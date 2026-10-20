# Responsive GUI delivery plan

Tracking issue: [#376](https://github.com/TucanoTechnology/TucanoTime/issues/376).
Status: planning only; no responsive interface changes have shipped in this slice.
The issue owns scope and completion; this document records implementation guidance.

## Goal and boundaries

Adapt the existing GUI to phones and tablets using Canonical Vanilla patterns,
without redesigning or repeatedly realigning the desktop experience. Retain
Vanilla 4.59.0, local assets, Ubuntu typography, spacing tokens, control order,
element IDs, routes and business behaviour. No backend/API changes are needed.

The [existing design record](ubuntu-design.md) remains applicable. The
[historical migration tracker](ubuntu-migration-plan.md) is not a new work queue;
its component migrations must not be repeated, and historical issue status must
be checked on GitHub before relying on it.

## Evidence and first check

- `web/styles.css`: `.sidebar` reserves 232px on desktop. At 720px and below,
  `.app` becomes a column and `.tabs` wraps into a top bar, not a menu drawer.
- The stylesheet also has 480px and 1000px rules. Audit these against Vanilla's
  breakpoints rather than mechanically replacing every number.
- `web/index.html`: one `#tabs` vertical ARIA tablist owns section navigation;
  account controls, colour mode and the setup wizard also live in the sidebar.
- Registers already use Vanilla mobile-card tables in several views. The Week
  grid and invoice register have local scrolling constraints; preserve their
  meaning instead of shrinking text until they fit.
- Reports, Approvals and advanced billing are currently hidden. Responsive work
  must not expose them or change role-based visibility.

Working hypothesis: a shared responsive navigation container can recover narrow
screen space without changing the dimensions or order of desktop page controls.
The first discriminating check is a before/after browser geometry and screenshot
comparison at 1280px and 1440px, paired with navigation/reflow checks at 375px and
768px. Reject a shell approach that moves existing desktop controls unexpectedly.
These are planned checks, not results of a browser audit in this planning slice.

## Proposed navigation contract

| Viewport width | Proposed behaviour | Acceptance constraint |
| --- | --- | --- |
| Below 620px | Compact brand/menu bar; closed-by-default overlay drawer | Main content gets the available width; opening the drawer does not reflow it |
| 620px to below 1036px | Compact rail with explicit expand/pin control | Works with touch and keyboard, not hover alone; icon labels remain accessible |
| 1036px and wider | Expanded rail | Preserve existing 232px rail and desktop page geometry |

This follows Vanilla's **application layout** defaults (620px and 1036px), not
the ordinary navigation component's single 1036px menu threshold. Confirm the
contract in R1/R2 before implementation. If the compact tablet rail compromises
discoverability, propose a drawer through the tablet range for review rather
than silently changing this contract.

Use one navigation DOM and retain its existing tab/route contracts. A full
`l-application` conversion is not a prerequisite: first check whether the pinned
Sass application-layout/side-navigation mixins can be adopted without resetting
desktop geometry. Verify every class against the compiled vendor bundle. Any
additional mixins need the existing asset generator, manifest hashes and licences;
no CDN, new runtime framework, vendor upgrade or global reset.

Drawer requirements: labelled Vanilla menu/close icons, `aria-expanded` and
`aria-controls`, Escape and backdrop dismissal, deliberate initial focus,
contained focus and inert background while modal, focus restoration, background
scroll locking and recovery. Closing after route selection should place focus
in the destination heading/content; dismissal should return it to the opener.
Resizing must clear stale modal state and leave focus on a visible control.
Keep theme, account and setup actions reachable; closed controls cannot be tabbable.

## Ordered delivery slices

Each row is a work package, not permission for a whole-GUI PR. Page families
must be split into individual view PRs once shared behaviour is stable.

| Slice | Work and owning surfaces | Exit evidence |
| --- | --- | --- |
| R1: Baselines | Extend `gui-test/browser-smoke.py` using deterministic fixtures, fixed clock/data, stable font loading and disabled animation; inventory current routes, themes and member/admin visibility | Approved desktop screenshots, overflow exception list and breakpoint contract before layout edits |
| R2: Navigation | Shared shell in `web/index.html`, `web/styles.css` and existing navigation initialisation; preserve IDs, roving focus and routing | Mobile open/close/route/back/resize journeys, tablet touch/pin checks and unchanged desktop baselines |
| R3: Shared controls | Heading/filter/action rows, Settings tabs, forms, native dialogs and popovers | Controls wrap in existing reading order; labels, errors, focus and popup actions remain visible in short and narrow viewports |
| R4: Timesheets | Day first, then Week, then Calendar; review each separately | Entry create/edit/copy/timer flows work; seven-day context, totals, lock indicators and keyboard editing survive local grid scrolling |
| R5: Registers | Customers, Projects, Tasks, Expenses; one view per PR | Reuse labelled mobile-card rows where appropriate; long names, filters, sorting, pagination, receipts and row actions remain usable |
| R6: Billing and Settings | Invoices register, wizard, editor, preview and manual payment; then each Settings route; include sign-in and setup | No lost invoice totals/actions; dialogs fit; document/PDF/print geometry stays separate from responsive editor geometry |
| R7: Consolidation | Complete route/role/theme matrix and review only obsolete responsive rules | Automated gates green; maintainer visual sign-off recorded for every affected page |

R1 precedes R2; R3 follows the accepted shell; page work follows R3. Include
invoices' hidden advanced-billing sections in the visibility regression checks,
not in a feature-enablement exercise. Apply the same rule to Reports/Approvals.

First implementation tickets:
- [R1: baselines and regression gates (#377)](https://github.com/TucanoTechnology/TucanoTime/issues/377)
- [R2: accessible drawer and collapsible navigation (#378)](https://github.com/TucanoTechnology/TucanoTime/issues/378), blocked by #377

## Preventing realignment churn

1. Record current desktop screenshots and control bounds before changing CSS.
   Fix fixture data, viewport, browser, fonts and theme so comparisons are useful.
2. Each PR names affected routes/selectors and its allowed visual differences.
   Do not change fonts, global spacing, field widths or page composition alongside
   responsiveness. Keep new geometry scoped to the relevant breakpoint/component.
3. Compare before/after at matching widths. Desktop control bounds should stay
   within 1 CSS px unless a documented exception is approved; use a calibrated
   screenshot tolerance for rasterisation, not automatic baseline replacement.
4. Do not conceal overflow with document-level `overflow-x: hidden`, duplicate
   mobile markup, reorder controls visually, or accumulate per-page magic offsets.
5. Review one page in both themes before starting the next. Unapproved visual
   differences block the slice; small conventional commits reference the ticket.

## Verification contract

- Widths: 320, 375, 620, 768, 1036, 1280 and 1440 CSS px; additionally test
  619/620/621 and 1035/1036/1037, plus surviving app breakpoint boundaries.
- Test portrait/landscape, short screens, keyboard and touch, reduced motion,
  light/dark, 200% text enlargement and 400% browser zoom/reflow. Exercise actual
  browser zoom separately from viewport resizing.
- Assert document `scrollWidth <= clientWidth + 1`. Permit horizontal scrolling
  only for explicitly listed two-dimensional tables/grids in labelled,
  keyboard-accessible regions; never lose data, totals, headers or actions.
- Cover empty/loading/error/populated states and long customer/project names,
  email addresses, invoice numbers, validation messages and large money values.
- Reuse existing GUI tests/helpers. jsdom verifies state/ARIA/routing but cannot
  prove layout: extend the existing real Firefox browser gate for geometry,
  interactions and screenshot artifacts on failure. Run embedded and dev assets.
- Required implementation gates: `cargo fmt --all -- --check`,
  `cargo clippy --all-targets -- -D warnings`, `cargo test --locked --all-targets`,
  `cargo build --locked`, GUI jsdom and contrast suites, live `gui.mjs`, and
  `browser-smoke.py` in both modes. Use isolated temporary data, never live `Data`.
- Capture per-view maintainer sign-off and remaining exceptions on the issue;
  close only after verified PRs are reviewed and merged.

## Design resources

- [Canonical design and Vanilla](https://design.ubuntu.com/vanilla)
- [Vanilla application layout](https://vanillaframework.io/docs/layouts/application)
- [Vanilla breakpoints](https://vanillaframework.io/docs/settings/breakpoint-settings)
- [Figma core components](https://www.figma.com/community/file/1435297834108003391/vanilla-core-component-library)
- [Figma assets/icons](https://www.figma.com/community/file/1435313921650671696/vanilla-assets-library)

For Figma work, duplicate both libraries and connect the assets library to icon
instance swapping. This is a design handoff step, not a runtime dependency;
the application continues using its locally vendored Vanilla icons. No Figma
file has been created or connected as part of this planning change.