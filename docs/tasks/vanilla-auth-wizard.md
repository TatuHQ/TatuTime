# Task: make auth window + first-customer wizard share the Vanilla card look (#229)

## Goal
The first-run **sign-in window** (`#auth-form`) and the **add-first-customer
setup wizard** (`#wizard-dialog`) must look the **same**, built on **pure
Ubuntu / Vanilla Framework** components (`p-card`, `p-card__header`,
`p-card__content`, `p-button--positive`), reusing `p-*` classes instead of
bespoke CSS. Keep every existing element id so the jsdom + GUI smoke suites
keep passing.

## Verified ground truth (Vanilla 4.59.0, `web/vendor/ubuntu/components.css`)
- `.p-card`: `border: 1.5px solid var(--vf-color-border-default)`,
  `background: var(--vf-color-background-default)`, `padding: calc(1rem - 1px)`,
  **`overflow: auto`**, `margin-bottom: 1.5rem`.
- `.p-card__header`: `border-bottom: 1px solid var(--vf-color-border-low-contrast)`,
  `padding-bottom: 1rem`.
- `.p-card__content`: no layout rules of its own (only last-child margin reset).
- `.p-button--positive` uses `--vf-color-button-positive-*` tokens.
- Vanilla `button` carries default margin `0 1rem 1.18rem 0` → needs
  `u-no-margin--bottom` (and the app already resets `button` margins globally).
- Focus rings use `outline-offset: -0.1875rem` (inset) → `p-card`'s
  `overflow:auto` does **not** clip them.
- **No** `.p-form` / `.p-field` / `.p-form-validation` classes exist in this
  build → form fields stay on the existing bespoke `.field` pattern.
  _(Superseded: #232 increment 1 later extended the bundle with the `vf-b-forms`
  family, so `p-form__group` / `p-form__label` / `p-form__control` /
  `p-form-help-text` / `p-form-validation__message` exist now — see
  [`../ubuntu-design.md`](../ubuntu-design.md). The `.p-field` name never existed
  in any 4.59 build.)_
- Utility classes that exist: `.p-heading--1..4`, `.u-no-margin--bottom`.

## Scope decision
Rebuild **only** these two windows on Vanilla `p-card` + `p-button--positive`.
The surrounding bespoke `.card` sites (entry form, invoice columns, settings
forms, …) are **out of scope** here and follow in the staged migration tracked
in `docs/ubuntu-design.md`.

## Ids that must not change
Auth: `auth-form`, `auth-title`, `auth-sub`, `auth-name-field`, `auth-name`,
`auth-email`, `auth-password`, `auth-error`, `auth-submit`, `sso-box`,
`sso-buttons`.
Wizard: `wizard-dialog`, `wz-title`, `wz-step-0..3`, `wz-cust-name`,
`wz-cust-currency`, `wz-cust-rate`, `wz-project-*`, `wz-later`, `wz-back`,
`wz-next`, `wz-status`, `wz-error`, `wz-done-text`.

## Verification gate
- `cargo fmt --all -- --check` && `cargo clippy --all-targets -- -D warnings`
- `cargo test --all-targets`
- `cd gui-test && node --test *.test.mjs && node contrast.mjs`
- Build release, run with a temp data dir on :8099, then `node gui.mjs`.
- Check **both** light and dark themes; confirm focus rings aren't clipped.

## Process notes for the next session
- **Minimal diffs, exact-match edits.** Large files (`web/styles.css`, ~3k
  lines) must be edited with exact-match `python3` heredocs; verify
  `git diff --stat` stays tiny after every edit.
- No new dependencies; `web/` is vanilla HTML/CSS/JS embedded via rust-embed.
- CSS/HTML only — no `openapi.json` change expected.
