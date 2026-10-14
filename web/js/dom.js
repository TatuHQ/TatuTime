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
  // #191: `data-positive`/`data-icon` are the stable contract (a label or id
  // rename can no longer silently strip the icon/colour); the old submit,
  // class, id-allowlist and label-matching rules remain fallbacks.
  const positive = button.dataset.positive === '1'
    || button.type === 'submit' || button.classList.contains('primary')
    || ['customer-new', 'expense-new', 'user-new', 'iw-save', 'ed-save'].includes(button.id);
  if (!button.classList.contains('link') && !button.classList.contains('danger')) {
    button.classList.add(positive ? 'p-button--positive' : 'p-button');
  }
  if (button.querySelector('[class*="p-icon--"]')) return;
  const text = button.textContent.trim();
  const icon = button.dataset.icon
    ?? (text.startsWith('+') ? 'plus'
      : text === 'Edit' ? 'edit'
        : text === 'Delete' ? 'delete'
          : text.startsWith('Copy') ? 'copy'
            : text === 'Start timer' ? 'play'
              : text === 'Stop' ? 'stop' : null);
  if (icon) {
    if (!button.dataset.icon && text.startsWith('+')) {
      const label = [...button.childNodes].find((node) => node.nodeType === Node.TEXT_NODE && node.textContent.trim().startsWith('+'));
      if (label) label.textContent = label.textContent.replace(/^\s*\+\s*/, '');
    }
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

export function showFormError(el, err) {
  const fields = err.payload?.error?.fields;
  let text = err.message || 'The request failed.';
  if (Array.isArray(fields) && fields.length) {
    text = fields.map((f) => `${f.field}: ${f.message}`).join(' · ');
  }
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

