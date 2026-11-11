// The phone shell is sized from `visualViewport`, not only from CSS.
//
// `100dvh` is the CSS answer, but Chrome Android can report a dynamic height
// that lags its URL-bar animation, which left a blank strip under the quick
// navigation. `initViewportHeight()` mirrors the visible height into
// `--app-height`; this test pins that mirror and its resize listener.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { JSDOM } from 'jsdom';
import { loadAppSource } from './load-app.mjs';

const html = readFileSync(new URL('../web/index.html', import.meta.url), 'utf8');
const app = loadAppSource();

function setup(height) {
    const dom = new JSDOM(html, { runScripts: 'outside-only', url: 'http://localhost/' });
    const listeners = [];
    dom.window.visualViewport = {
        height,
        addEventListener: (type, listener) => {
            if (type === 'resize') listeners.push(listener);
        },
    };
    dom.window.eval(`${app}\ninitViewportHeight();`);
    return { dom, listeners };
}

const appHeight = (dom) => dom.window.document.documentElement.style.getPropertyValue('--app-height');

test('the phone shell height follows the visual viewport', () => {
    const { dom, listeners } = setup(700);
    assert.equal(appHeight(dom), '700px', 'the initial visible height becomes the shell height');

    dom.window.visualViewport.height = 760; // the URL bar hides
    listeners.forEach((listener) => listener());
    assert.equal(appHeight(dom), '760px', 'hiding the URL bar grows the shell with it');

    dom.window.visualViewport.height = 733.6;
    listeners.forEach((listener) => listener());
    assert.equal(appHeight(dom), '734px', 'fractional heights round to whole pixels');
    dom.window.close();
});

test('a browser without visualViewport keeps the 100dvh fallback', () => {
    const dom = new JSDOM(html, { runScripts: 'outside-only', url: 'http://localhost/' });
    dom.window.eval(`${app}\ninitViewportHeight();`);
    assert.equal(appHeight(dom), '', 'no CSS variable is invented without a visual viewport');
    dom.window.close();
});
