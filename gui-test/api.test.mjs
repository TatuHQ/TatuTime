// api.js: what a failed request says. The app's one error shape always wins, but
// a body that is not that shape (a proxy page, or a server too old to know the
// route at all) must still produce something diagnosable — "HTTP 405" alone left
// a developer guessing between a wrong verb, a stale server and a proxy. That is
// exactly how a stale process serving an un-mounted `PUT /api/expenses/{id}`
// looked from the dialog.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { JSDOM } from 'jsdom';
import { loadAppSource } from './load-app.mjs';

const html = readFileSync(new URL('../web/index.html', import.meta.url), 'utf8');
const app = loadAppSource();

function load() {
    const dom = new JSDOM(html, { runScripts: 'outside-only', url: 'http://localhost/' });
    dom.window.eval(app);
    return dom.window;
}

/// A minimal `Response` stand-in: the helper only reads `status` and `headers`.
const response = (status, allow = null) => ({
    status,
    headers: { get: (name) => (name.toLowerCase() === 'allow' ? allow : null) },
});

test('a failure without the JSON error shape names the request and what is accepted', () => {
    const window = load();
    const err = window.requestFailure('PUT', '/expenses/abc', response(405, 'GET, DELETE'), null);
    assert.equal(err.status, 405);
    assert.equal(
        err.message,
        'PUT /api/expenses/abc failed: HTTP 405 (this endpoint accepts GET, DELETE)',
    );
    assert.equal(err.payload, null);
    window.close();
});

test('the server\'s own message wins, and a missing Allow is left out', () => {
    const window = load();
    const shaped = window.requestFailure(
        'POST',
        '/expenses',
        response(422, null),
        { error: { code: 'validation_failed', message: 'payload failed validation' } },
    );
    assert.equal(shaped.message, 'payload failed validation');
    assert.equal(shaped.payload.error.code, 'validation_failed');
    const bare = window.requestFailure('GET', '/nope', response(500, null), null);
    assert.equal(bare.message, 'GET /api/nope failed: HTTP 500');
    window.close();
});

// The second half of the same confusion: the server *does* answer the route,
// but with the app's error shape refusing a key the page just sent. A stale
// process owns that too — a debug build reads `web/` from disk per request
// (rust-embed), so its modules are current while its Rust is not.
test('a field the server does not know is named as a stale server, not a typo', () => {
    const window = load();
    const el = window.document.getElementById('expense-error');
    window.showFormError(el, {
        payload: {
            error: {
                code: 'validation_failed',
                fields: [{ field: 'original_amount_minor', message: 'unknown field' }],
            },
        },
    });
    assert.match(el.textContent, /^original_amount_minor: unknown field/);
    assert.match(el.textContent, /older than this page/);
    window.close();
});

test('an ordinary field error keeps the server\'s own wording', () => {
    const window = load();
    const el = window.document.getElementById('expense-error');
    window.showFormError(el, {
        payload: {
            error: {
                code: 'validation_failed',
                fields: [{ field: 'amount_minor', message: 'at most 100000000' }],
            },
        },
    });
    assert.equal(el.textContent, 'amount_minor: at most 100000000');
    window.close();
});
