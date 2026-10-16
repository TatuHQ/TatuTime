// #264 follow-up regression guard: module *evaluation order*.
//
// web/ is a real 17-module ES-module graph served as-is, so the browser
// evaluates it depth-first, post-order — never in the order the files happen to
// be listed in web/app.js. `gui-test/load-app.mjs` deliberately concatenates the
// modules in file order, which is exactly the order that hides ordering bugs:
//
//   * a specifier that does not resolve to a served URL (the entry point lives
//     at the web root while its modules live in web/js/), so the browser gets
//     the JSON 404 shape for `/api.js` and refuses it for its MIME type, or
//   * a module whose top-level wiring reads a `const`/`let` binding exported by
//     a module the browser has not finished evaluating yet — a cycle through
//     the graph's leaf module puts that leaf last, and the first consumer dies
//     with "can't access lexical declaration … before initialization".
//
// Either one leaves the GUI inert with no visible cause: HTML and CSS paint,
// but every button, the navigation and all data are dead. Both were invisible
// to the jsdom suites, which never resolve URLs and never execute real modules.
//
// This guard walks the real graph, resolves every specifier against the file
// that declares it, then evaluates the modules in the browser's own order in a
// jsdom window. A missing module or an order-dependent binding fails here.
// Style follows the sibling static guards [`dialog-display.test.mjs`](./dialog-display.test.mjs),
// [`css-tokens.test.mjs`](./css-tokens.test.mjs) and
// [`condition-waits.test.mjs`](./condition-waits.test.mjs): no browser, no new
// dependency.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { existsSync, readFileSync } from 'node:fs';
import { JSDOM, VirtualConsole } from 'jsdom';

const WEB = new URL('../web/', import.meta.url);
const webUrl = (rel) => new URL(rel, WEB);
const read = (rel) => readFileSync(webUrl(rel), 'utf8');

// The entry point: the single `<script type="module">` the shell loads.
function moduleEntry(html) {
    const tag = html.match(/<script[^>]*type=["']module["'][^>]*>/i);
    assert.ok(tag, 'index.html must load a <script type="module"> entry point');
    const src = tag[0].match(/src=["']([^"']+)["']/i);
    assert.ok(src, 'the module entry point must have a src');
    return src[1].replace(/^\//, '');
}

// Graph specifiers of a module, in source order: `import './x.js'`,
// `import { a } from './y.js'` and `export … from './z.js'`. Only relative
// specifiers name another file of web/.
function specifiersOf(source) {
    const specifiers = [];
    for (const raw of source.split('\n')) {
        const line = raw.trim();
        if (!/^(import|export)\b/.test(line)) continue;
        const literal = line.match(/['"]([^'"]+)['"]\s*;?\s*$/);
        if (literal && literal[1].startsWith('.')) specifiers.push(literal[1]);
    }
    return specifiers;
}

// Resolve a relative specifier against the module that declares it, exactly as
// the browser resolves it against that module's URL.
function resolve(fromRel, specifier) {
    const base = fromRel.slice(0, fromRel.lastIndexOf('/') + 1);
    const segments = [];
    for (const segment of (base + specifier).split('/')) {
        if (segment === '' || segment === '.') continue;
        if (segment === '..') segments.pop();
        else segments.push(segment);
    }
    return segments.join('/');
}

test('#264: the GUI module graph resolves and evaluates in browser order', () => {
    const entry = moduleEntry(read('index.html'));

    // 1. Follow every specifier from the entry; each hop must be a real file.
    const graph = new Map();
    const missing = [];
    const build = (rel) => {
        if (graph.has(rel)) return;
        const deps = [];
        for (const specifier of specifiersOf(read(rel))) {
            const dep = resolve(rel, specifier);
            deps.push(dep);
            if (!existsSync(webUrl(dep))) missing.push(`${rel} imports '${specifier}' → web/${dep} does not exist`);
        }
        graph.set(rel, deps);
        for (const dep of deps) build(dep);
    };
    build(entry);
    assert.deepEqual(missing, [], `module specifiers must resolve to served files:\n  ${missing.join('\n  ')}`);

    // The published split (#264) is 19 modules plus the entry (fx joined for the
    // expense currency conversion, the receipt half split out of expenses.js in
    // #332): a guard that stops following imports (or an entry that imports
    // nothing) must not pass.
    assert.ok(
        graph.size >= 20,
        `expected the entry + its 19 modules, walked ${graph.size}: ${[...graph.keys()].join(', ')}`,
    );

    // 2. Depth-first, post-order — the evaluation order the HTML spec defines.
    //    Pre-marking each module on entry puts a cycle's entry point last, which
    //    is where the browser evaluates it too.
    const order = [];
    const visited = new Set();
    const visit = (rel) => {
        if (visited.has(rel)) return;
        visited.add(rel);
        for (const dep of graph.get(rel)) visit(dep);
        order.push(rel);
    };
    visit(entry);
    assert.equal(order.length, graph.size, 'every module is evaluated exactly once');
    assert.equal(order[order.length - 1], entry, 'the entry point evaluates last');

    // 3. Evaluate the graph in that order, as one scope (jsdom cannot run real
    //    ES modules; stripping imports/exports mirrors load-app.mjs). Import and
    //    export prefixes only ever start a line, so this cannot mangle code.
    const source = order
        .map((rel) => `// ── web/${rel}\n${read(rel).replace(/^import\b[^\n]*\n/gm, '').replace(/^export[ \t]+/gm, '')}`)
        .join('\n')
        + '\nglobalThis.__tucanoGraphEvaluated = true;\n';

    const problems = [];
    const vc = new VirtualConsole();
    vc.on('jsdomError', (error) => problems.push(error.message));
    const dom = new JSDOM(read('index.html'), {
        runScripts: 'outside-only',
        pretendToBeVisual: true,
        url: 'http://localhost/',
        virtualConsole: vc,
    });
    let failure = null;
    try {
        dom.window.eval(source);
    } catch (error) {
        failure = error;
    }
    assert.equal(
        failure && failure.message,
        null,
        `the module graph must evaluate cleanly in browser order; failed at:\n${(failure && failure.stack) || ''}`,
    );
    assert.equal(
        dom.window.__tucanoGraphEvaluated,
        true,
        'evaluation must reach the end of the graph (a module threw part-way)',
    );
    dom.window.close();
});
