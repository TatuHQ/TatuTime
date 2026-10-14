// TucanoTime GUI entry point. Pure presentation layer: every value comes from
// and goes to the JSON API. The server is the only actor that touches storage.
// All user data is inserted with textContent (never innerHTML) so stored
// notes/names cannot execute as markup.
//
// Split into ES modules with no build step (#264): the browser loads this file
// as <script type="module"> and pulls in every module below; rust-embed serves
// each one as-is. Import order mirrors the original top-to-bottom layout so
// module evaluation order (and therefore behaviour) is preserved.
import './api.js';
import './dom.js';
import './seq.js';
import './state.js';
import './route.js';
import './calendar.js';
import './timesheet.js';
import './hierarchy.js';
import './invoices.js';
import './billing.js';
import './expenses.js';
import './settings.js';
import './timer.js';
import './notifications.js';
import './boot.js';
import './wizard.js';
import './auth.js';
