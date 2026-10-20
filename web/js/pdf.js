import { el } from './dom.js';

export async function pdfDeadline(promise, milliseconds = 15000, message = 'PDF preview timed out. Open the PDF or retry.') {
  let timer;
  try {
    return await Promise.race([promise, new Promise((resolve, reject) => {
      timer = setTimeout(() => reject(new Error(message)), milliseconds);
    })]);
  } finally {
    clearTimeout(timer);
  }
}

export async function startPdfWorker(pdfjs, loadFallback = () => import('/vendor/pdfjs/pdf.worker.mjs')) {
  let worker;
  try {
    worker = new pdfjs.PDFWorker();
    await pdfDeadline(worker.promise, 3000, 'PDF worker did not become ready.');
    return worker;
  } catch {
    worker?.destroy();
    globalThis.pdfjsWorker = await pdfDeadline(loadFallback());
    const fallback = new pdfjs.PDFWorker();
    try {
      await pdfDeadline(fallback.promise, 3000);
      return fallback;
    } catch (error) {
      fallback.destroy();
      throw error;
    }
  }
}

export async function renderPdfDocument(host, bytes, current = () => true) {
  const pdfjs = await pdfDeadline(import('/vendor/pdfjs/pdf.mjs'));
  pdfjs.GlobalWorkerOptions.workerSrc = '/vendor/pdfjs/pdf.worker.mjs';
  const worker = await startPdfWorker(pdfjs);
  if (!current()) { worker.destroy(); return () => {}; }
  const task = pdfjs.getDocument({
    data: bytes.slice(), worker, isEvalSupported: false, useWasm: false,
    standardFontDataUrl: '/vendor/pdfjs/standard_fonts/',
  });
  const destroy = () => { task.destroy().catch(() => {}); worker.destroy(); };
  let document;
  try { document = await pdfDeadline(task.promise); }
  catch (error) { destroy(); throw error; }
  if (!current()) { destroy(); return () => {}; }
  let pageNumber = 1;
  let zoom = 0;
  let generation = 0;
  let rendering = null;
  const status = el('span', { attrs: { role: 'status', 'aria-live': 'polite' } });
  const pages = el('div', { cls: 'pdf-pages', attrs: { tabindex: '0', role: 'region', 'aria-label': 'Invoice PDF page' } });
  const previous = el('button', { type: 'button', cls: 'p-button', data: { icon: 'chevron-left' }, attrs: { 'aria-label': 'Previous PDF page', title: 'Previous PDF page' }, on: { click: () => change(-1) } });
  const next = el('button', { type: 'button', cls: 'p-button', data: { icon: 'chevron-right' }, attrs: { 'aria-label': 'Next PDF page', title: 'Next PDF page' }, on: { click: () => change(1) } });
  const scale = el('select', { attrs: { 'aria-label': 'PDF zoom' } }, [
    el('option', { value: '0', text: 'Fit width' }),
    ...[100, 150, 200].map(value => el('option', { value: String(value / 100), text: `${value}%` })),
  ]);
  const toolbar = el('div', { cls: 'pdf-toolbar' }, [previous, status, next, scale]);
  const render = async () => {
    const revision = ++generation;
    rendering?.cancel();
    const page = await pdfDeadline(document.getPage(pageNumber));
    if (!current() || revision !== generation) return;
    const base = page.getViewport({ scale: 1 });
    const factor = zoom || Math.max(.2, (host.clientWidth - 8) / base.width);
    const viewport = page.getViewport({ scale: factor });
    const canvas = el('canvas', { attrs: { role: 'img', 'aria-label': `Invoice page ${pageNumber} of ${document.numPages}`, 'data-pdf-page': String(pageNumber) } });
    const pixels = Math.min(window.devicePixelRatio || 1, 2);
    canvas.width = Math.ceil(viewport.width * pixels);
    canvas.height = Math.ceil(viewport.height * pixels);
    canvas.style.width = `${viewport.width}px`;
    canvas.style.height = `${viewport.height}px`;
    rendering = page.render({ canvasContext: canvas.getContext('2d'), viewport,
      transform: pixels === 1 ? null : [pixels, 0, 0, pixels, 0, 0] });
    try { await pdfDeadline(rendering.promise); } catch (error) {
      if (error.name === 'RenderingCancelledException') return;
      throw error;
    }
    const content = await pdfDeadline(page.getTextContent());
    if (!current() || revision !== generation) return;
    const text = el('p', { cls: 'sr-only', text: content.items.map(item => item.str).join(' ') });
    pages.replaceChildren(canvas, text);
    status.textContent = `Page ${pageNumber} of ${document.numPages}`;
    previous.disabled = pageNumber === 1;
    next.disabled = pageNumber === document.numPages;
  };
  const report = error => { status.textContent = `Preview unavailable: ${error.message}`; };
  const change = delta => { pageNumber = Math.max(1, Math.min(document.numPages, pageNumber + delta)); render().catch(report); };
  scale.addEventListener('change', () => { zoom = Number(scale.value); render().catch(report); });
  host.replaceChildren(toolbar, pages);
  try { await render(); } catch (error) { destroy(); throw error; }
  return () => { generation++; rendering?.cancel(); destroy(); };
}