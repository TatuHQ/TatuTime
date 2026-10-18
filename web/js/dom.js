// dom.js — TucanoTime GUI module (ES-module split of web/app.js, #264).
// Moved from the original monolithic app.js; behaviour is unchanged.
//
// This is the graph's leaf module: it imports nothing, so the browser
// evaluates it before every consumer and `$` is always initialised by the time
// another module's top-level wiring runs. Importing a view module here (as the
// split originally did for `iw`/`ed`, which this file never uses) puts dom.js
// inside a cycle, makes it evaluate *last*, and the first consumer's `$('…')`
// then throws "can't access lexical declaration '$' before initialization".

export const $ = (id) => document.getElementById(id);

// ---------------------------------------------------------------- helpers --
//
// `el` replaces the hand-rolled `createElement` chains the renderers copied
// around (#102): ~350 lines of identical boilerplate become one builder. As
// everywhere in this file, values enter the DOM through `textContent` only —
// never innerHTML.

export function el(tag, opts = {}, children = []) {
  const node = document.createElement(tag);
  if (opts.cls) node.className = opts.cls;
  if (opts.text !== undefined) node.textContent = opts.text;
  if (opts.type) node.type = opts.type;
  if (opts.value !== undefined) node.value = opts.value;
  // #188: top-level `style` used to be dropped silently, which flattened the
  // invoice bar chart (heights computed in JS never reached the DOM). Applied
  // as a plain attribute — callers pass server-independent numbers only.
  if (opts.style) node.setAttribute('style', opts.style);
  if (opts.attrs) {
    for (const [k, v] of Object.entries(opts.attrs)) node.setAttribute(k, v);
  }
  if (opts.data) {
    for (const [k, v] of Object.entries(opts.data)) node.dataset[k] = v;
  }
  if (opts.on) {
    for (const [evt, fn] of Object.entries(opts.on)) node.addEventListener(evt, fn);
  }
  for (const child of [].concat(children)) if (child) node.appendChild(child);
  if (tag === 'button') decorateUbuntuButton(node);
  return node;
}

export function ubuntuIcon(name) {
  const icon = document.createElement('i');
  icon.className = `p-icon--${name}`;
  icon.setAttribute('aria-hidden', 'true');
  return icon;
}

export function decorateUbuntuButton(button) {
  if (button.getAttribute('role') === 'tab') return;
  // #232 increment 3: a chip's own controls (its dismiss button) carry the
  // official Vanilla chip skin — the generic button decorator must not stack
  // `p-button`/icons on top of them.
  if (button.classList.contains('p-chip__dismiss')) return;
  // #191/#232 phase 2e: `data-positive`/`data-icon` are the *only* contract.
  // The old submit-type / class / id-allowlist / label-matching fallbacks — and
  // the label-mutating "+"-strip they brought — are gone: every creation site
  // states what it wants, so a label or id rename can no longer silently strip
  // a button's colour, and a data-driven label (a customer called "Copy Center")
  // can no longer invent a glyph. Decoration is now a pure function of the two
  // explicit attributes.
  const positive = button.dataset.positive === '1';
  if (!button.classList.contains('link') && !button.classList.contains('danger')) {
    button.classList.add(positive ? 'p-button--positive' : 'p-button');
  }
  if (button.querySelector('[class*="p-icon--"]')) return;
  const icon = button.dataset.icon;
  if (icon) {
    const graphic = ubuntuIcon(icon);
    if (positive) graphic.classList.add('is-dark');
    button.prepend(graphic);
  }
}

// The shared select-refill pattern: clear, then append options described as
// `{ value, text, selected }`. Selection happens by property, as before.
// One action-cell button builder for every table row (#189: previously a
// byte-identical local const existed twice, and five renderers re-inlined it).
export function rowAction(cls, text, fn, title, icon) {
  return el('button', {
    cls,
    type: 'button',
    text,
    // #232 phase 2b: the icon is stated at the creation site instead of being
    // derived from the label by `decorateUbuntuButton`, so the decorator's
    // label heuristic is redundant (deleted in phase 2e).
    data: icon ? { icon } : undefined,
    attrs: title ? { title } : {},
    on: { click: fn },
  });
}

// #232 increment 3: the official status pill. Vanilla 4.59 deprecates
// `p-status-label` — "the status label has been merged into the Chip component"
// (canonical/vanilla-framework#5535) — so a status renders as `p-chip` +
// `p-chip__value`. `is-inline` drops the chip's block margin so the pill sits on
// a table cell / note baseline; the app's pre-existing binary emphasis (the old
// `.badge.on` vs `.badge`) maps to `p-chip--positive` vs the neutral chip. The
// label enters through `textContent` only.
export function statusChip(text, emphasised = false) {
  return el('span', { cls: `p-chip is-inline${emphasised ? ' p-chip--positive' : ''}` }, [
    el('span', { cls: 'p-chip__value', text }),
  ]);
}

export function fillSelect(select, options) {
  select.textContent = '';
  for (const o of options) {
    const opt = el('option', { value: o.value, text: o.text });
    if (o.selected) opt.selected = true;
    select.appendChild(opt);
  }
}

// ---------------------------------------------------------------- dialogs --
//
// One inline <dialog> replaces window.confirm/prompt/alert (#102): focus is
// trapped by the native element, Enter accepts, Escape cancels, and the jsdom
// harness drives the same buttons a real user sees. Resolution is wired to
// the buttons directly (no `method="dialog"`), so behaviour does not depend
// on the host implementing dialog form submission.
//
// #232 increment 4: every popup renders the official Vanilla `p-modal` overlay
// (`p-modal` on the native <dialog>, a `p-modal__dialog` box inside it); only
// the markup classes moved — the native element still owns the top layer,
// ::backdrop, inert background and focus trap.

// The native <dialog> returns focus to the control that opened it on close;
// jsdom (and the getAttribute fallback path) does not, so make the contract
// explicit for every dialog: wrap show/close to remember the opener and focus
// it again on close. Idempotent, so wiring twice is harmless.
export function initDialogFocusReturn() {
  for (const dlg of document.querySelectorAll('dialog')) {
    if (dlg.dataset.focusWired) continue;
    dlg.dataset.focusWired = '1';
    const show = dlg.showModal;
    const close = dlg.close;
    if (typeof show === 'function') {
      dlg.showModal = function (...args) {
        if (!(this.open === true || this.hasAttribute('open'))) this.__opener = document.activeElement;
        return show.apply(this, args);
      };
    }
    if (typeof close === 'function') {
      dlg.close = function (...args) {
        const wasOpen = this.open === true || this.hasAttribute('open');
        const result = close.apply(this, args);
        const opener = this.__opener;
        this.__opener = null;
        if (wasOpen && opener && opener.isConnected && typeof opener.focus === 'function') opener.focus();
        return result;
      };
    }
  }
}

export function dlgSettle(result) {
  const dlg = $('app-dialog');
  const resolve = dlg.__resolve;
  if (!resolve) return; // already settled (button + Escape race)
  dlg.__resolve = null;
  if (typeof dlg.close === 'function') dlg.close();
  else dlg.removeAttribute('open');
  resolve(result);
}

export function showDialog({ title, message, input = null, value = '', okLabel = 'OK', cancelLabel = null }) {
  return new Promise((resolve) => {
    $('dlg-title').textContent = title;
    $('dlg-message').textContent = message;
    const wrap = $('dlg-input-wrap');
    wrap.hidden = !input;
    if (input) $('dlg-input-label').textContent = input;
    $('dlg-input').value = value;
    const ok = $('dlg-ok');
    ok.textContent = okLabel;
    const cancel = $('dlg-cancel');
    cancel.textContent = cancelLabel || 'Cancel';
    cancel.hidden = !cancelLabel;
    const dlg = $('app-dialog');
    dlg.__resolve = resolve;
    dlg.__inputMode = !!input;
    if (typeof dlg.showModal === 'function') dlg.showModal();
    else dlg.setAttribute('open', ''); // jsdom fallback: no modal support yet
    (input ? $('dlg-input') : ok).focus();
  });
}

export function askConfirm(message, okLabel = 'Confirm') {
  return showDialog({
    title: 'Please confirm',
    message,
    okLabel,
    cancelLabel: 'Cancel',
  }).then((r) => r === 'ok');
}

// Resolves with the typed string, or null when cancelled (like prompt()).
export function askPrompt(message, defaultValue = '') {
  return showDialog({ title: 'Input', message, input: message, value: defaultValue }).then(
    (r) => (r === 'cancel' ? null : r),
  );
}

export function askAlert(message) {
  return showDialog({ title: 'Notice', message });
}

export function initDialog() {
  const dlg = $('app-dialog');
  $('dlg-ok').addEventListener('click', () => {
    dlgSettle(dlg.__inputMode ? $('dlg-input').value : 'ok');
  });
  // The promise always resolves with a settle MARKER; `askConfirm` maps it to
  // a boolean and `askPrompt` maps it to null-on-cancel, so a cancel marker
  // can never collide with a falsy user value.
  $('dlg-cancel').addEventListener('click', () => dlgSettle('cancel'));
  dlg.addEventListener('cancel', () => dlgSettle('cancel')); // native Escape
  dlg.addEventListener('keydown', (e) => {
    if (e.key === 'Escape') dlgSettle('cancel'); // hosts without dialog keyboard
    else if (e.key === 'Enter') {
      e.preventDefault();
      $('dlg-ok').click();
    }
  });
}

export function announce(msg) {
  $('live-region').textContent = msg;
}

// ---------------------------------------------------------- single flight --
//
// QA-01: a create form's submit control stays live while its request is in
// flight, so a double click (or a second Enter) used to POST twice and create
// the record twice. `singleFlight` is the one guard the create-form handlers
// share: the first run marks the button busy -- disabled + `aria-busy` so it
// still reads sensibly to assistive tech -- and a second run returns immediately
// without issuing anything. `finally` restores the control, so a rejected save
// never leaves the form permanently stuck.
//
// The button is also the guard's identity: `dataset.busy` is the flag, so two
// different forms can never block each other.

export async function singleFlight(button, run) {
  if (button && button.dataset.busy === '1') return undefined;
  if (button) {
    button.dataset.busy = '1';
    button.disabled = true;
    button.setAttribute('aria-busy', 'true');
  }
  try {
    return await run();
  } finally {
    if (button) {
      delete button.dataset.busy;
      button.disabled = false;
      button.removeAttribute('aria-busy');
    }
  }
}

// ------------------------------------------------- stale panels (#QA-10) --
//
// A refresh that fails must not leave the panel looking current, and must not
// blank it either: the rows on screen are the last known good data. Every view
// loader runs through `panelRefresh`, which renders one visible `role="alert"`
// banner inside that view's panel on failure -- naming the error, saying the
// data is stale -- with a Retry button that re-runs the same loader. A later
// successful load removes the banner, so it clears itself automatically.
//
// One banner per panel: repeated failures replace it, and two loaders can never
// stack two banners in the same view. The banner reuses the app's existing
// notification vocabulary (`p-notification p-notification--caution`), so no new
// skin is introduced.

const panelViews = new Map(); // panelId -> { panelId, label, run, loaded, banner }

function panelView(panelId, label, run) {
  let view = panelViews.get(panelId);
  if (!view) {
    view = { panelId, label, run, loaded: false, banner: null };
    panelViews.set(panelId, view);
  }
  view.label = label;
  view.run = run;
  return view;
}

function clearPanelBanner(view) {
  if (!view.banner) return;
  view.banner.remove();
  view.banner = null;
}

function showPanelBanner(view, err) {
  const host = $(view.panelId);
  if (!host) return;
  clearPanelBanner(view);
  // The stale claim is only true once something loaded: before the first
  // successful load there is no last-known-good data to speak of.
  const stale = view.loaded
    ? 'Showing data from the last successful load.'
    : 'No data has been loaded yet.';
  const retry = el('button', {
    type: 'button',
    cls: 'p-button',
    text: 'Retry',
    attrs: { 'aria-label': `Retry refreshing ${view.label}` },
  });
  retry.addEventListener('click', () => {
    if (retry.dataset.busy === '1') return;
    retry.dataset.busy = '1';
    retry.disabled = true;
    retry.setAttribute('aria-busy', 'true');
    // A retry either clears the banner (success) or replaces it (still
    // failing), so the busy button is never left behind in either case.
    panelRefresh(view.panelId, view.label, view.run).catch(() => {});
  });
  const banner = el('div', {
    cls: 'p-notification p-notification--caution',
    attrs: { role: 'alert', 'data-panel': view.panelId },
  }, [
    el('div', { cls: 'p-notification__content' }, [
      el('p', { cls: 'p-notification__title', text: `Refresh failed -- ${err.message}` }),
      el('p', { cls: 'p-notification__message', text: stale }),
      retry,
    ]),
  ]);
  host.prepend(banner);
  view.banner = banner;
}

/// Run one view's loader with the shared stale-panel contract. The loader's
/// rejection is re-thrown unchanged: `refreshPanel` still announces it on the
/// live region, and direct callers keep the behaviour they had before.
export async function panelRefresh(panelId, label, run) {
  const view = panelView(panelId, label, run);
  try {
    const result = await run();
    view.loaded = true;
    clearPanelBanner(view);
    return result;
  } catch (err) {
    showPanelBanner(view, err);
    throw err;
  }
}

/// The reason the API gives for a key it does not know at all — the
/// `deny_unknown_fields` rejection in `api::deser_error`, `FieldError::new(field,
/// "unknown field")`. serde only refuses a key that was *actually sent*, so this
/// is never a bad field name in the payload: it means the process answering
/// predates the field the page just sent. The modules and the binary can drift
/// apart because the sheets are served from disk while the Rust is fixed at
/// compile time — `TUCANO_WEB_DIR` does that in any profile (the dev asset path,
/// src/lib.rs) and rust-embed does it in a debug build — so a long-running
/// server hands out today's GUI over yesterday's contract. Naming that is the
/// difference between "original_amount_minor: unknown field" sending the reader
/// hunting for a typo in the dialog and a restart.
const UNKNOWN_FIELD = 'unknown field';

export function showFormError(el, err) {
  const fields = err.payload?.error?.fields;
  const code = err.payload?.error?.code;
  let text = err.message || 'The request failed.';
  if (Array.isArray(fields) && fields.length) {
    // A failure serde could not tie to a key (a body that is not JSON, or a
    // value of the wrong type — see api::deser_error) has an empty field: the
    // reason stands alone rather than after a dangling colon.
    text = fields
      .map((f) => (f.field ? `${f.field}: ${f.message}` : f.message))
      .join(' · ');
    if (fields.some((f) => f.message === UNKNOWN_FIELD)) {
      text += ' — the running server is older than this page: restart or rebuild it.';
    }
  } else if (typeof err.status === 'number' && err.status >= 500) {
    // A 5xx is the server's own failure. `ApiError::internal` deliberately
    // withholds the cause (paths and raw filesystem errors never reach a
    // client), so name the contract code and the status — the details exist
    // only in the server log. Nothing from the response body beyond the
    // code is printed, and only `textContent` is ever assigned.
    const label = code ? `${code}, HTTP ${err.status}` : `HTTP ${err.status}`;
    text += ` (${label}) — check the server log for details.`;
  }
  // A machine-readable hook for tests and screenshots: the code without
  // parsing the prose above. `clearFormError` removes it with the text.
  if (code) el.dataset.errorCode = code;
  else delete el.dataset.errorCode;
  el.textContent = text;
  el.hidden = false;
  const form = el.closest('form');
  if (form && Array.isArray(fields)) {
    const prefix = el.id.replace(/-error$/, '');
    const aliases = { default_rate_minor: 'default-rate', cost_rate_minor: 'cost-rate', rate_minor: 'rate' };
    for (const field of fields) {
      const suffix = aliases[field.field] || field.field.replaceAll('_', '-');
      const input = [...form.elements].find((control) => control.name === field.field || control.id === `${prefix}-${suffix}`);
      if (!input) continue;
      input.setAttribute('aria-invalid', 'true');
      const descriptions = new Set((input.getAttribute('aria-describedby') || '').split(' ').filter(Boolean));
      descriptions.add(el.id);
      input.setAttribute('aria-describedby', [...descriptions].join(' '));
      input.dataset.errorMessage = el.id;
    }
  }
}

export function clearFormError(el) {
  el.textContent = '';
  el.hidden = true;
  delete el.dataset.errorCode;
  for (const input of el.closest('form')?.elements || []) {
    if (input.dataset?.errorMessage !== el.id) continue;
    input.removeAttribute('aria-invalid');
    const descriptions = (input.getAttribute('aria-describedby') || '').split(' ').filter((id) => id && id !== el.id);
    if (descriptions.length) input.setAttribute('aria-describedby', descriptions.join(' '));
    else input.removeAttribute('aria-describedby');
    delete input.dataset.errorMessage;
  }
}

export function isoDate(d) {
  const y = d.getFullYear();
  const m = String(d.getMonth() + 1).padStart(2, '0');
  const day = String(d.getDate()).padStart(2, '0');
  return `${y}-${m}-${day}`;
}

export function addDays(dateStr, n) {
  const [y, m, d] = dateStr.split('-').map(Number);
  const dt = new Date(y, m - 1, d); // local midnight: calendar-day math with local getters
  dt.setDate(dt.getDate() + n);
  return isoDate(dt);
}

export function mondayOf(dateStr) {
  const [y, m, d] = dateStr.split('-').map(Number);
  const dt = new Date(y, m - 1, d);
  const dow = dt.getDay(); // 0 Sun .. 6 Sat
  const shift = (dow === 0 ? -6 : 1) - dow;
  return addDays(dateStr, shift);
}

export function formatMoney(minor) {
  return (minor / 100).toFixed(2);
}

