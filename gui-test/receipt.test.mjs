// Receipts (#23): what the register's Receipt column says, what the preview
// shows, and the two shapes the dialog refuses instead of storing.
//
// The browser gate covers the live image preview; these are the paths it cannot
// reach cheaply — the PDF framing (a blob URL, which jsdom stubs), the refusals
// that keep a hollow receipt out of storage, and the row that has no receipt.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { JSDOM } from 'jsdom';
import { loadAppSource } from './load-app.mjs';
import { waitFor } from './wait-for.mjs';

const html = readFileSync(new URL('../web/index.html', import.meta.url), 'utf8');
const app = loadAppSource();

const RECEIPTS = [
  { id: 'x1', date: '2026-10-01', customer_id: 'c1', amount_minor: 12500, currency: 'EUR',
    billable: true, receipt_name: 'scan.png', receipt_b64: 'aGVsbG8=' },
  { id: 'x2', date: '2026-10-02', customer_id: 'c1', amount_minor: 2000, currency: 'EUR',
    billable: true, receipt_name: 'hollow.pdf', receipt_b64: '' },
  { id: 'x3', date: '2026-10-03', customer_id: 'c1', amount_minor: 500, currency: 'EUR', billable: true },
];

// The dialog's own wiring (the preview's Close/Download, the form's submit) is
// boot.js's, so the harness boots the app the way the browser does: one eval that
// also hands this suite the handful of bindings it drives, then DOMContentLoaded.
async function setup(expenses = RECEIPTS) {
  const dom = new JSDOM(html, { runScripts: 'outside-only', url: 'http://localhost/' });
  const { window } = dom;
  window.HTMLDialogElement.prototype.showModal = function showModal() { this.open = true; };
  window.HTMLDialogElement.prototype.close = function close() { this.open = false; };
  const urls = { created: [], revoked: [] };
  window.URL.createObjectURL = () => {
    const url = `blob:jsdom-${urls.created.length + 1}`;
    urls.created.push(url);
    return url;
  };
  window.URL.revokeObjectURL = (url) => urls.revoked.push(url);
  window.eval(`${app}
window.f = { api, state, refreshExpenses, previewReceipt, resetExpenseForm, $ };
window.posts = [];
const empty = { entries: [], customers: [], projects: [], submissions: [], expenses: [],
  claims: [], notifications: [], categories: [], item_types: [], invoices: [],
  users: [], schedules: [], retainers: [], timer: null };
api.post = async (path, body) => { window.posts.push({ path, body }); return {}; };
api.put = api.post;
api.get = async (path) => {
  if (path === '/auth/status') return { initialised: true };
  if (path === '/auth/me') return { id: 'a1', name: 'Admin', email: 'a@x.test', role: 'admin' };
  if (path === '/auth/sso/providers') return { providers: [] };
  if (path === '/expenses') return { expenses: ${JSON.stringify(expenses)} };
  return empty;
};
`);
  window.document.dispatchEvent(new window.Event('DOMContentLoaded', { bubbles: true }));
  await waitFor(() => window.document.getElementById('version').textContent === 'TucanoTime', {
    message: 'the app to finish booting',
  });
  return { dom, window, document: window.document, urls };
}

/// A window left open keeps the runner's event loop alive, so every test closes
/// on the way out whatever an assertion does.
function harness(t, parts) {
  t.after(() => parts.window.close());
  return parts;
}

test('the Receipt column names the stored file, and says when there is none', async (t) => {
  const { window, document } = harness(t, await setup());
  await window.f.refreshExpenses();
  const cells = [...document.querySelectorAll('#expense-table tbody tr td[data-heading="Receipt"]')];
  assert.equal(cells.length, 3, 'one receipt cell per row');

  const [withBytes, hollow, none] = cells;
  assert.equal(withBytes.querySelector('button').textContent.trim(), 'scan.png',
    'the row links to the file it stores');
  assert.match(withBytes.querySelector('button').getAttribute('aria-label'), /Preview the receipt for 2026-10-01/);
  assert.match(hollow.textContent, /The attached receipt is empty/,
    'a name stored with no bytes is not offered as a receipt');
  assert.equal(hollow.querySelector('button'), null, 'and it is not a link');
  assert.match(none.textContent, /No receipt attached/);
  assert.equal(none.querySelector('button'), null);
});

test('an image receipt previews from its own bytes, and closing releases them', async (t) => {
  const { window, document, urls } = harness(t, await setup());
  await window.f.refreshExpenses();
  const link = document.querySelector('#expense-table tbody td[data-heading="Receipt"] button');
  const createdBefore = urls.created.length;
  link.dispatchEvent(new window.Event('click', { bubbles: true }));

  assert.equal(document.getElementById('receipt-dialog').open, true, 'the preview opens');
  assert.equal(document.getElementById('receipt-dialog-title').textContent, 'scan.png');
  const image = document.getElementById('receipt-image');
  assert.ok(image, 'an image receipt renders as an image');
  assert.equal(image.getAttribute('src'), 'data:image/png;base64,aGVsbG8=');
  assert.equal(urls.created.length, createdBefore, 'a data: URL needs no object URL to own');
  assert.equal(document.getElementById('receipt-preview').querySelectorAll('iframe').length, 0);

  document.getElementById('receipt-close').dispatchEvent(new window.Event('click', { bubbles: true }));
  assert.equal(document.getElementById('receipt-dialog').open, false);
  assert.equal(document.getElementById('receipt-preview').childNodes.length, 0, 'the document leaves with it');
});

test('a PDF receipt is framed from a blob URL the preview revokes when it closes (#CSP frame-src)', async (t) => {
  const pdf = { id: 'p1', date: '2026-10-04', customer_id: 'c1', amount_minor: 900, currency: 'EUR',
    billable: true, receipt_name: 'scan.pdf', receipt_b64: 'aGVsbG8=' };
  const { window, document, urls } = harness(t, await setup([pdf]));
  await window.f.refreshExpenses();
  document.querySelector('#expense-table tbody td[data-heading="Receipt"] button')
    .dispatchEvent(new window.Event('click', { bubbles: true }));

  const frame = document.getElementById('receipt-frame');
  assert.ok(frame, 'anything that is not an image is framed');
  assert.equal(frame.getAttribute('title'), 'Receipt scan.pdf');
  const own = urls.created.filter((url) => url === frame.getAttribute('src'));
  assert.equal(own.length, 1, 'the frame is handed a blob URL of its own');

  document.getElementById('receipt-close').dispatchEvent(new window.Event('click', { bubbles: true }));
  assert.deepEqual(urls.revoked, own, 'the blob URL is released when the preview closes');
});

test('the dialog refuses an empty file instead of storing a hollow receipt', async (t) => {
  const { window, document } = harness(t, await setup());
  const input = document.getElementById('expense-receipt');
  const error = document.getElementById('expense-error');
  // The submit handler is async (it reads the file before it posts), so every
  // assertion waits for the state it reads rather than a tick (#265).
  const attach = (file) => {
    Object.defineProperty(input, 'files', { value: [file], configurable: true });
    input.dispatchEvent(new window.Event('change', { bubbles: true }));
    document.getElementById('expense-form')
      .dispatchEvent(new window.Event('submit', { bubbles: true, cancelable: true }));
  };

  window.posts = [];
  window.f.resetExpenseForm();
  document.getElementById('expense-amount').value = '12.50';
  document.getElementById('expense-currency').value = 'EUR';

  attach(new window.File([], 'empty.pdf', { type: 'application/pdf' }));
  await waitFor(() => /empty\.pdf is empty \(0 bytes\)/.test(error.textContent), {
    message: 'the empty file to be refused',
  });
  assert.equal(window.posts.length, 0, 'nothing is sent, so no hollow receipt can be stored');

  // Over the 3 MB the dialog and the API both document.
  attach(new window.File([new window.Blob(['x'.repeat(3_000_001)])], 'huge.pdf'));
  await waitFor(() => /huge\.pdf is 2\.9 MB/.test(error.textContent), {
    message: 'the oversized file to be refused',
  });
  assert.equal(window.posts.length, 0, 'an oversized receipt is refused before the round trip');

  // A file that carries bytes still saves, receipt and all.
  attach(new window.File([new window.Blob(['hello'])], 'scan.png', { type: 'image/png' }));
  await waitFor(() => window.posts.some((p) => p.path === '/expenses'), {
    message: 'the save to reach the API',
  });
  const saved = window.posts.find((p) => p.path === '/expenses');
  assert.equal(saved.body.receipt_name, 'scan.png');
  assert.equal(saved.body.receipt_b64, 'aGVsbG8=');
});
