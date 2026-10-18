// TucanoTime GUI entry point. Pure presentation layer: every value comes from
// and goes to the JSON API. The server is the only actor that touches storage.
// All user data is inserted with textContent (never innerHTML) so stored
// notes/names cannot execute as markup.
//
// Split into ES modules with no build step (#264): the browser loads this file
// as <script type="module"> and pulls in every module below; rust-embed serves
// each one as-is. Import order mirrors the original top-to-bottom layout so
// module evaluation order (and therefore behaviour) is preserved.
//
// This entry point lives at the web root (/app.js) while the modules it pulls
// in live in web/js/, so every specifier is './js/<name>.js' — resolved against
// /app.js that is the served URL /js/<name>.js. A bare './<name>.js' would
// resolve to /<name>.js, which the server answers with the JSON 404 shape; the
// browser then refuses the module for its non-JS MIME type and the whole graph
// — hence the entire GUI — never evaluates.
import './js/api.js';
import './js/dom.js';
import './js/seq.js';
import './js/state.js';
import './js/route.js';
import './js/calendar.js';
import './js/timesheet.js';
import './js/hierarchy.js';
import './js/invoices.js';
import './js/billing.js';
import './js/receipt.js';
import './js/expenses.js';
import './js/fx.js';
import './js/settings.js';
import './js/timer.js';
import './js/notifications.js';
import './js/boot.js';
import './js/wizard.js';
import './js/setup.js';
import './js/auth.js';
