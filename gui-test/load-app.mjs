// Loads the real GUI ES modules as one classic script for jsdom (#264).
//
// jsdom cannot execute ES modules, so the harness evaluates the module sources
// concatenated in dependency order with the import/export syntax stripped —
// recreating the exact single scope the original monolithic web/app.js ran in.
// Behaviour is unchanged; only the file layout moved. Keep MODULES in the same
// order as web/app.js's imports (and the original top-to-bottom layout) so
// top-level initialisation order is preserved.
import { readFileSync } from 'node:fs';

const MODULES = [
    'api', 'dom', 'seq', 'state', 'pdf', 'route', 'calendar', 'timesheet', 'hierarchy',
    'invoices', 'billing', 'receipt', 'expenses', 'fx', 'settings', 'timer', 'notifications',
    'boot', 'wizard', 'setup', 'auth',
];

export function loadAppSource() {
    return MODULES
        .map((name) => readFileSync(new URL(`../web/js/${name}.js`, import.meta.url), 'utf8'))
        .join('\n')
        // Imports are single-line and export prefixes are line-leading; stripping
        // them yields the original flat, single-scope script.
        .replace(/^import\b[^\n]*\n/gm, '')
        .replace(/^export[ \t]+/gm, '');
}
