// fx.js — currency conversion for the expense dialog and the top-bar finder.
//
// Pure presentation: every rate comes from `GET /api/fx?from=&to=` and is never
// invented here. The endpoint answers `{from, to, rate, date, source}`, where
// `rate` is units of `to` per one unit of `from` and `date` is the day the
// published table was dated (`null` for a currency converted with itself, which
// needs no table at all). When no table can be loaded it fails closed with 503
// `fx_unavailable` — the dialog then keeps its rate field editable and the
// finder says it has no rate, rather than showing a guessed one.
//
// Money is integer minor units everywhere (server, storage, this module): the
// rate is a decimal *string*, parsed here only to multiply, and rounded exactly
// once, at the end (#11).


import { api } from './api.js';
import { $, announce, formatMoney } from './dom.js';
import { loadProjects, state } from './state.js';

// ---------------------------------------------------------------- maths ---

/// Upper-cased ISO-4217 code, or '' when the text is not three letters (the
/// server validates the same shape, so this only spares a round trip).
export function currencyCode(raw) {
  const code = String(raw || '').trim().toUpperCase();
  return /^[A-Z]{3}$/.test(code) ? code : '';
}

/// An entered amount as minor units (cents). `NaN` when the field holds no
/// number, so callers can tell "nothing typed" from "zero".
export function toMinor(raw) {
  const value = Number.parseFloat(raw);
  return Number.isFinite(value) ? Math.round(value * 100) : NaN;
}

/// `minor` converted at `rate` (a decimal string), in minor units — or `NaN`
/// when either side is unusable. The rate is never rounded, only the result.
export function convertMinor(minor, rate) {
  const value = Number.parseFloat(rate);
  if (!Number.isFinite(value) || value <= 0 || !Number.isFinite(minor)) return NaN;
  return Math.round(minor * value);
}

/// One rate, or `null` when the pair cannot be quoted right now (the published
/// table is unavailable, or the currency is not in it). Callers keep their own
/// editable rate instead of a guess.
export async function fetchRate(from, to) {
  try {
    const answer = await api.get(`/fx?from=${encodeURIComponent(from)}&to=${encodeURIComponent(to)}`);
    return {
      rate: Number.parseFloat(answer.rate),
      date: answer.date || '',
      source: answer.source || '',
    };
  } catch {
    return null;
  }
}

// ------------------------------------------------------- expense dialog ---

/// The currency the picked customer/project bills in: the project's own
/// currency when a project is picked (every project carries one, #11), else the
/// customer's default. '' while no customer is picked.
export function billedCurrency() {
  const cid = $('expense-customer').value;
  const customer = state.customers.find((c) => c.id === cid);
  const code = $('expense-project').value;
  const project = code ? (state.projectsByCustomer[cid] || []).find((p) => p.code === code) : null;
  if (project && project.currency) return currencyCode(project.currency);
  return customer ? currencyCode(customer.currency) : '';
}

/// The pair the rate field currently holds. A refetch happens only when the
/// user actually changes the currency or the customer/project — not on every
/// keystroke.
let expensePair = '';

/// The amount currency we last prefilled from the pick. The prefill follows a
/// customer/project change only while the field still holds that value: once the
/// user chooses a currency of their own the field is theirs, and switching
/// customer must not silently reinterpret the amount they entered.
let expensePrefilled = '';

/// Called when the dialog opens or is dismissed, so the next draft starts from
/// a clean rate instead of the previous expense's.
export function resetExpenseConversion() {
  expensePair = '';
  expensePrefilled = '';
  $('expense-rate').value = '';
  $('expense-rate-date').value = '';
}

/// The billed value the dialog currently implies, from what its fields hold —
/// no request. Cleared when the amount is already in the billed currency.
export function showConverted() {
  const out = $('expense-converted');
  const entered = currencyCode($('expense-currency').value);
  const billed = billedCurrency();
  const minor = toMinor($('expense-amount').value);
  if (!entered || !billed || entered === billed || !Number.isFinite(minor)) {
    out.textContent = '';
    return;
  }
  const converted = convertMinor(minor, $('expense-rate').value.trim());
  out.textContent = Number.isFinite(converted)
    ? `${entered} ${formatMoney(minor)} = ${billed} ${formatMoney(converted)}`
    : `Enter a rate to see the ${billed} value of ${entered} ${formatMoney(minor)}.`;
}

/// Keeps the dialog's conversion fields in step with the amount's currency and
/// the picked customer/project: the billed currency decides whether anything is
/// converted at all, the rate is fetched once per pair and stays editable, and
/// the readout shows the value in the billed currency.
export async function refreshExpenseConversion() {
  const rateInput = $('expense-rate');
  const dateInput = $('expense-rate-date');
  const note = $('expense-rate-note');
  const label = $('expense-rate-label');
  const out = $('expense-converted');
  const cid = $('expense-customer').value;
  const project = $('expense-project').value;
  // Only the projects carry a currency pick of their own, and they are loaded
  // per customer — warm that cache before asking what the pick bills in.
  if (cid && project) {
    try {
      await loadProjects(cid);
    } catch {
      // Leave `billedCurrency()` to fall back to the customer's default.
    }
  }
  const billed = billedCurrency();
  const chosen = currencyCode($('expense-currency').value);
  if (billed && chosen !== billed && (chosen === '' || chosen === expensePrefilled)) {
    // The amount is in the customer/project currency unless the user says
    // otherwise: prefill it — and follow a later pick — so the common case needs
    // no conversion at all. A currency the user chose is never overwritten.
    $('expense-currency').value = billed;
    expensePrefilled = billed;
  } else if (chosen) {
    expensePrefilled = ''; // the user owns this field from here on
  }
  const entered = currencyCode($('expense-currency').value);
  const convert = Boolean(entered && billed && entered !== billed);
  rateInput.disabled = !convert;
  dateInput.disabled = !convert;
  label.textContent = convert ? `Exchange rate (${entered} \u2192 ${billed})` : 'Exchange rate';
  out.textContent = '';
  if (!convert) {
    expensePair = '';
    rateInput.value = '';
    dateInput.value = '';
    note.textContent = !entered
      ? "Enter the amount's currency to see the value in the billed currency."
      : !billed
        ? 'Pick a customer (or a project) to see the value billed in its currency.'
        : `Billed in ${billed} — the amount is already in the customer/project currency.`;
    return;
  }
  const pair = `${entered}>${billed}`;
  if (pair !== expensePair) {
    expensePair = pair;
    rateInput.value = '';
    dateInput.value = '';
    note.textContent = `Looking up the ${entered}/${billed} reference rate…`;
    const fetched = await fetchRate(entered, billed);
    if (expensePair !== pair) return; // superseded by a newer pick
    if (fetched && Number.isFinite(fetched.rate)) {
      // Six decimals keep the value inside the contract's pattern: the rate is a
      // string on the wire and in storage, never a float (#11).
      rateInput.value = fetched.rate.toFixed(6);
      dateInput.value = fetched.date;
      note.textContent = fetched.date
        ? `Reference rate for ${fetched.date} (editable).`
        : 'No conversion needed.';
    } else {
      note.textContent =
        `No published ${entered}/${billed} rate is available right now — enter the rate by hand.`;
    }
  }
  showConverted();
}

// --------------------------------------------------------- top-bar finder ---

/// The masthead converter: an amount and a pair, converted with the same
/// endpoint the dialog uses.
export async function convertFx(evt) {
  if (evt) evt.preventDefault();
  const out = $('fx-result');
  const from = currencyCode($('fx-from').value);
  const to = currencyCode($('fx-to').value);
  const minor = toMinor($('fx-amount').value);
  if (!from || !to) {
    out.textContent = 'Enter two three-letter currency codes (e.g. USD and EUR).';
    return;
  }
  if (!Number.isFinite(minor)) {
    out.textContent = 'Enter the amount to convert.';
    return;
  }
  const fetched = await fetchRate(from, to);
  if (!fetched || !Number.isFinite(fetched.rate)) {
    out.textContent = `No published ${from}/${to} rate is available right now.`;
    announce(`No published ${from}/${to} reference rate is available right now.`);
    return;
  }
  const converted = convertMinor(minor, fetched.rate);
  const at = fetched.date ? ` at the ${fetched.date} reference rate` : '';
  out.textContent = `${from} ${formatMoney(minor)} = ${to} ${formatMoney(converted)}${at}.`;
}
