// receipt.js — TatuTime GUI module (ES-module split of web/app.js, #264).
// The receipt half, moved out of web/js/expenses.js (#332); behaviour is
// unchanged.

import { $, el } from './dom.js';

// ---------------------------------------------------------------- receipt ---
//
// The API keeps a receipt inline with its expense (#23): the file's name and its
// bytes as base64. The dialog owns one draft receipt — the file just picked, or
// the one the record being edited already carries, because a PUT replaces the
// whole record and an untouched receipt has to travel back with the payload.

let draftReceipt = null;

/// The draft receipt the expense dialog is holding: read and written through this
/// pair so the dialog's module (expenses.js) never reaches into this module's
/// state — the dependency runs one way (#332).
export function getDraftReceipt() {
  return draftReceipt;
}

/// Replace the draft: the file just picked, the record being edited, or `null`.
export function setDraftReceipt(x) {
  draftReceipt = x;
}

/// The record the open preview is showing, so its Download button saves exactly
/// what the dialog displays.
let previewRecord = null;

export const RECEIPT_NOTE = 'Images and PDFs up to 3 MB, stored with the expense.';

/// The largest receipt the API stores, in *file* bytes: the server refuses more
/// than 4,000,000 characters of base64 (`domain::validate_expense_input`), which
/// is a 3 MB file once encoded. Checking here names the file and its size instead
/// of spending a round trip to be told the payload was too large.
export const MAX_RECEIPT_BYTES = 3_000_000;

/// `bytes` as a short human size, for the two refusals below.
function receiptSize(bytes) {
  return bytes >= 1024 * 1024
    ? `${(bytes / (1024 * 1024)).toFixed(1)} MB`
    : `${Math.max(1, Math.round(bytes / 1024))} KB`;
}

/// A picked file, shaped the way the API stores it. Two shapes are refused here
/// rather than stored: an empty file (a name with no bytes is a receipt the
/// register cannot open and the user cannot tell from a missing one — storing it
/// silently is how a hollow record gets written) and one over the server's cap.
export async function readReceipt(file) {
  if (!file.size) {
    throw new Error(`${file.name} is empty (0 bytes) — attach a file with content.`);
  }
  if (file.size > MAX_RECEIPT_BYTES) {
    throw new Error(
      `${file.name} is ${receiptSize(file.size)} — the largest receipt is 3 MB.`,
    );
  }
  const data = await new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onerror = () => reject(new Error('That file could not be read.'));
    reader.onload = () => resolve(String(reader.result || ''));
    reader.readAsDataURL(file);
  });
  const comma = data.indexOf(',');
  const b64 = comma >= 0 ? data.slice(comma + 1) : '';
  if (!b64) throw new Error(`${file.name} could not be read — attach it again.`);
  return { name: file.name, b64 };
}

/// Hand a stored receipt to the browser as a download under its own name.
export function downloadReceipt(x) {
  if (!x || !x.receipt_b64) return;
  const link = document.createElement('a');
  link.href = `data:${receiptMime(x.receipt_name)};base64,${x.receipt_b64}`;
  link.download = x.receipt_name || `receipt-${x.date}`;
  link.rel = 'noopener';
  document.body.appendChild(link);
  link.click();
  link.remove();
}

/// The type a stored receipt is handed back under, so the browser picks its own
/// viewer. Storage keeps the file name (#23) and nothing else, so the extension
/// is the only hint there is; anything unknown stays opaque and downloads.
function receiptMime(name) {
  const ext = String(name || '').toLowerCase().split('.').pop();
  return {
    png: 'image/png',
    jpg: 'image/jpeg',
    jpeg: 'image/jpeg',
    gif: 'image/gif',
    webp: 'image/webp',
    bmp: 'image/bmp',
    svg: 'image/svg+xml',
    tif: 'image/tiff',
    tiff: 'image/tiff',
    heic: 'image/heic',
    pdf: 'application/pdf',
  }[ext] || 'application/octet-stream';
}

/// The stored base64 as the bytes a blob can carry.
function receiptBlob(b64, mime) {
  const binary = atob(b64);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i += 1) bytes[i] = binary.charCodeAt(i);
  return new Blob([bytes], { type: mime });
}

/// The blob URL the open preview is showing, so closing revokes it: a receipt of
/// a few megabytes must not be held alive by a dialog the user has dismissed.
let previewUrl = '';

function releasePreviewUrl() {
  if (previewUrl) URL.revokeObjectURL(previewUrl);
  previewUrl = '';
}

/// Open a stored receipt in its preview: an image as an image (a data: URL, which
/// the CSP already allows for images), anything else — a PDF — framed from a blob
/// URL the browser renders with its own viewer (`frame-src 'self' blob:`). The
/// bytes already travel with the record, so the preview costs no request, and the
/// dialog's Download button saves the very bytes it is showing.
export function previewReceipt(x) {
  if (!x || !x.receipt_b64) return;
  const name = x.receipt_name || `receipt-${x.date}`;
  const mime = receiptMime(name);
  const host = $('receipt-preview');
  host.textContent = '';
  releasePreviewUrl();
  previewRecord = x;
  $('receipt-dialog-title').textContent = name;
  host.appendChild(
    mime.startsWith('image/')
      ? el('img', {
        attrs: { id: 'receipt-image', src: `data:${mime};base64,${x.receipt_b64}`, alt: `Receipt ${name}` },
      })
      : el('iframe', {
        attrs: { id: 'receipt-frame', src: previewUrl = URL.createObjectURL(receiptBlob(x.receipt_b64, mime)), title: `Receipt ${name}` },
      }),
  );
  $('receipt-dialog').showModal();
  $('receipt-close').focus();
}

/// Let go of what the open preview holds: the blob URL it minted, the record it
/// would download, and the document element in the dialog.
function clearReceiptPreview() {
  releasePreviewUrl();
  previewRecord = null;
  $('receipt-preview').textContent = '';
}

/// Dismiss the preview. The Close button runs this directly rather than leaning on
/// the dialog's `close` event, so the teardown happens on the path the user takes
/// — and the event keeps Escape and any other native dismissal just as clean.
export function closeReceiptPreview() {
  $('receipt-dialog').close();
  clearReceiptPreview();
}

/// Wire the preview dialog once, at boot: Download saves what is open, Close
/// dismisses it, and every close path releases the bytes.
export function initReceiptPreview() {
  $('receipt-close').addEventListener('click', closeReceiptPreview);
  $('receipt-download').addEventListener('click', () => downloadReceipt(previewRecord));
  $('receipt-dialog').addEventListener('close', clearReceiptPreview);
}

/// The register's receipt cell: the stored file's own name as the link that
/// opens the preview when the record carries bytes, and a dash when it does not
/// — announced, because "no receipt" is an answer, not an empty cell. A record
/// whose name was stored with no bytes (an empty file, before the dialog refused
/// them) says so rather than offering a preview of nothing.
export function receiptCell(x) {
  const name = x.receipt_name || `receipt-${x.date}`;
  if (x.receipt_b64) {
    return el('button', {
      cls: 'link',
      type: 'button',
      data: { icon: 'file' },
      text: name,
      attrs: {
        'aria-label': `Preview the receipt for ${x.date} (${name})`,
        title: `Preview ${name}`,
      },
      on: { click: () => previewReceipt(x) },
    });
  }
  return el('span', {}, [
    el('span', { attrs: { 'aria-hidden': 'true' }, text: '\u2014' }),
    el('span', {
      cls: 'sr-only',
      text: x.receipt_name ? 'The attached receipt is empty' : 'No receipt attached',
    }),
  ]);
}
