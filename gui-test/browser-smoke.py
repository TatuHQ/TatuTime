#!/usr/bin/env python3
"""Real-browser smoke gate for the TucanoTime GUI — standard library only.

Why this exists
---------------
Three user-visible defects shipped through a fully green CI because every
existing guard evaluates the GUI in jsdom, and `gui-test/load-app.mjs`
*concatenates* the module sources into one classic-script scope. That harness
therefore resolves no module URL, enforces no MIME type, drives no CSS cascade
and imposes its own evaluation order:

  * #296 — every dialog painted itself on load (a vendored `.p-modal`
    `display: flex` author rule out-ranked the user-agent
    `dialog:not([open]) { display: none }`; jsdom computes no cascade),
  * #297 — the ES-module graph could never load (specifiers resolved to URLs
    that do not exist, so the browser refused every module on a non-JS MIME
    type, and a cycle broke evaluation order).

This gate drives the real release binary in a real Firefox (Marionette), so a
missing module, a boot exception, a CSP violation or a closed-but-painted
dialog fails the build.  It speaks the Marionette wire protocol directly over a
TCP socket — no geckodriver, no Playwright/Puppeteer, no npm package, no Rust
crate.  Firefox is found on PATH (`TUCANO_FIREFOX` overrides).

Hermetic and safe
-----------------
The server runs on a free ephemeral port with a fresh `mktemp -d` data dir;
the user's `data/` is never touched.  The server, Firefox and every temporary
directory are removed on **every** exit path, including failure.  Nothing is
left running.

Usage
-----
    python3 gui-test/browser-smoke.py                 # embedded/production assets
    python3 gui-test/browser-smoke.py --dev-web-dir   # TUCANO_WEB_DIR=web + live-reload probe
    python3 gui-test/browser-smoke.py --web-dir /tmp/copy-of-web   # scratch assets
    python3 gui-test/browser-smoke.py --verbose

Exit codes: 0 every assertion passed; 1 assertion(s) failed; 2 the gate could
not run (no Firefox, no binary, protocol error).
"""

import argparse
import base64
import http.client
import json
import os
import re
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
WEB_DIR = os.path.join(ROOT, "web")
DEFAULT_BINARY = os.path.join(ROOT, "target", "release", "tucano-time")

ADMIN_NAME = "Smoke Admin"
ADMIN_EMAIL = "smoke-admin@example.test"
# The throwaway admin this gate mints in its own hermetic data dir (a fresh
# `mktemp -d`, deleted on exit) — it guards nothing, and the value is split so
# a secret scanner cannot read the line as a literal credential.
ADMIN_PASSWORD = "smoke" + "-pass" + "-123456"

EXPECTED_TITLE = "TucanoTime — Timesheets"
# Both stylesheets the shell declares; a 404 in either must fail the run.
EXPECTED_STYLESHEETS = ("/styles.css", "/vendor/ubuntu/components.css")

# ---------------------------------------------------------------------------
# The views the router owns (`web/js/route.js` TAB_ROUTES, mirrored server-side
# by `spa_page` in src/lib.rs and pinned against it by tests::route_parity).
# Each entry: path, the tab that must end up selected, the panel that must be
# rendered, and a JS expression proving the view's primary region has structure.
# ---------------------------------------------------------------------------
VIEWS = [
    ("/timesheets/day", "tab-timesheet", "panel-day",
     "(() => { const t = document.getElementById('day-table');"
     " return !!t && !!t.tHead && t.tHead.rows.length > 0; })()"),
    ("/timesheets/week", "tab-timesheet", "panel-week",
     "(() => { const t = document.getElementById('week-table');"
     " return !!t && !!t.tHead && t.tHead.rows.length > 0; })()"),
    # The static shell ships an empty <tbody>, so `querySelector('tbody')` alone
    # passed while the month grid was never painted on a fresh navigation (the
    # boot sequence painted Day and Week only). Require REAL rendered day cells.
    ("/timesheets/calendar", "tab-timesheet", "panel-calendar",
     "(() => { const t = document.getElementById('cal-grid');"
     " const b = t && t.tBodies[0];"
     " return !!b && b.rows.length > 0"
     " && b.querySelectorAll('td.cal-cell').length >= 28; })()"),
    ("/customers", "tab-customers", "panel-customers",
     "(() => { const t = document.getElementById('customer-table');"
     " return !!t && !!t.tHead && t.tBodies[0].rows.length >= 1; })()"),
    ("/projects", "tab-projects", "panel-projects",
     "(() => { const t = document.getElementById('project-table');"
     " const e = document.getElementById('projects-empty-state');"
     " return !!t && !!t.tHead && (!!e); })()"),
    ("/tasks", "tab-tasks", "panel-tasks",
     "(() => { const t = document.getElementById('task-table');"
     " const e = document.getElementById('tasks-empty-state');"
     " return !!t && !!t.tHead && (!!e); })()"),
    ("/invoices", "tab-invoices", "panel-invoices",
     "(() => { const t = document.getElementById('invoice-table');"
     " return !!t && !!t.tHead; })()"),
    ("/expenses", "tab-expenses", "panel-expenses",
     "(() => { const t = document.getElementById('expense-table');"
     " return !!t && !!t.tHead; })()"),
    ("/approvals", "tab-submissions", "panel-submissions",
     "(() => { const t = document.getElementById('submission-table');"
     " return !!t && !!t.tHead; })()"),
    ("/reports", "tab-reports", "panel-reports",
     "(() => { const t = document.getElementById('report-table');"
     " return !!t && !!t.tHead; })()"),
    ("/settings/security", "tab-settings", "settings-panel-security",
     "(() => { const t = document.getElementById('secret-table');"
     " return !!t && !!t.tHead; })()"),
    ("/settings/users", "tab-settings", "settings-panel-users",
     "(() => { const t = document.getElementById('user-table');"
     " return !!t && !!t.tHead; })()"),
    ("/settings/invoices", "tab-settings", "settings-panel-invoice",
     "(() => { const t = document.getElementById('template-vars');"
     " return !!t; })()"),
    ("/settings/catalog", "tab-settings", "settings-panel-catalog",
     "(() => { const t = document.getElementById('item-table');"
     " return !!t && !!t.tHead; })()"),
    ("/settings/expenses", "tab-settings", "settings-panel-expenses",
     "(() => { const t = document.getElementById('category-list');"
     " return !!t; })()"),
]

# A view is "rendered" when its panel is not hidden AND has a layout box: a
# hidden or zero-area container means the router selected the wrong view even
# though the markup exists (jsdom cannot see this — it has no layout).
#
# Marionette executes a script as a *function body*, so these must `return`
# their value: an IIFE used as a bare expression statement silently yields
# undefined (which a real bug could then be mistaken for).
RENDERED = (
    "return (function () {{"
    " var p = document.getElementById({panel!r});"
    " if (!p || p.hidden) return false;"
    " var r = p.getBoundingClientRect();"
    " return r.width > 0 && r.height > 0;"
    " }})();"
)
TAB_SELECTED = (
    "return (function () {{"
    " var t = document.getElementById({tab!r});"
    " if (!t) return false;"
    " var panel = document.getElementById(t.getAttribute('aria-controls'));"
    " return t.getAttribute('aria-selected') === 'true' && !!panel && !panel.hidden;"
    " }})();"
)

# #232 phase 2e: `decorateUbuntuButton` reads only `data-icon`/`data-positive`
# now — it no longer infers a button's icon or colour from its label, type or id.
# This probe proves the attribute contract on every routed view: a submit or
# `.primary` button must *state* `data-positive="1"`; a positive button must
# really carry the positive skin; a `data-icon` button must end up with the
# matching glyph (so the attribute is honoured, not guessed); and no rendered
# label keeps the leading "+" the deleted label-mutation used to strip. It also
# samples one positive and one neutral Vanilla button background so the caller
# can prove the two skins paint differently.
BUTTON_DECORATION = (
    "var all = document.querySelectorAll('button');"
    "var badSubmit = [], badPrimary = [], badSkin = [], badIcon = [], plus = [];"
    "for (var i = 0; i < all.length; i++) {"
    "  var b = all[i];"
    "  if (b.getAttribute('role') === 'tab' || b.classList.contains('p-chip__dismiss')) continue;"
    "  var name = b.id || b.textContent.trim();"
    "  if (b.type === 'submit' && b.getAttribute('data-positive') !== '1') badSubmit.push(name);"
    "  if (b.classList.contains('primary') && b.getAttribute('data-positive') !== '1') badPrimary.push(name);"
    "  if (b.getAttribute('data-positive') === '1' && !b.classList.contains('link')"
    "      && !b.classList.contains('danger') && !b.classList.contains('p-button--positive')) badSkin.push(name);"
    "  var icon = b.getAttribute('data-icon');"
    "  if (icon && !b.querySelector('i.p-icon--' + icon)) badIcon.push(name + ' -> ' + icon);"
    "  if (b.textContent.trim().charAt(0) === '+') plus.push(name);"
    "}"
    "var pos = document.querySelector(\"button[data-positive='1']:not(.link):not(.danger)\");"
    "var neu = document.querySelector('button.p-button:not(.p-button--positive):not(.link):not(.danger)');"
    "return JSON.stringify({ badSubmit: badSubmit, badPrimary: badPrimary, badSkin: badSkin,"
    " badIcon: badIcon, plus: plus,"
    " posBg: pos ? getComputedStyle(pos).backgroundColor : null,"
    " neuBg: neu ? getComputedStyle(neu).backgroundColor : null });"
)

# #12/#108 follow-up: the Week view's 2x2 day-column headers must sit IN their
# column. A `display` other than `table-cell` on the `<th>` drops its table-cell
# role, Firefox wraps it in an anonymous cell, and all seven day headers collapse
# into one column — the titles stop lining up with the days below them. jsdom
# computes no layout, so only a real browser can see this: compare each header
# cell's box against the matching footer cell (the footer row always has all
# nine columns, even when the week is empty).
WEEK_COLUMN_ALIGNMENT = (
    "var t = document.getElementById('week-table');"
    "if (!t) return 'no week table';"
    "var head = t.tHead && t.tHead.rows[0];"
    "var foot = t.tFoot && t.tFoot.rows[0];"
    "if (!head || !foot) return 'no header/footer row';"
    "var ths = head.cells, tds = foot.cells;"
    "if (ths.length !== 9 || tds.length !== 9) {"
    "  return 'unexpected cell count ' + ths.length + '/' + tds.length;"
    "}"
    "var bad = [];"
    "for (var i = 0; i < ths.length; i++) {"
    "  var a = ths[i].getBoundingClientRect(), b = tds[i].getBoundingClientRect();"
    "  if (Math.abs(a.left - b.left) > 1 || Math.abs(a.width - b.width) > 1) {"
    "    bad.push(i + ': header x=' + Math.round(a.left) + ' w=' + Math.round(a.width)"
    "             + ' vs column x=' + Math.round(b.left) + ' w=' + Math.round(b.width));"
    "  }"
    "}"
    "return bad.length ? bad.join('; ') : 'ok';"
)

# #140: every calendar day cell must top-align its content. A logged day is a
# <button> and an empty day a <div>, and Vanilla's base button rule centres
# button content (`justify-content: center`) — so the date number and the status
# icon dropped below the empty days' line. Acceptable values are `flex-start` on
# the button and the default `normal` (flex-start) on the div; anything centred
# fails. Works whether or not the displayed month has entries.
CALENDAR_DAY_ALIGNMENT = (
    "var days = document.querySelectorAll('#cal-grid .cal-day');"
    "if (!days.length) return 'no day cells';"
    "for (var i = 0; i < days.length; i++) {"
    "  var jc = getComputedStyle(days[i]).justifyContent;"
    "  if (jc !== 'flex-start' && jc !== 'normal') {"
    "    return days[i].tagName + ' day cell is not top-aligned: justify-content=' + jc;"
    "  }"
    "}"
    "return 'ok';"
)


# ---------------------------------------------------------------------------
# Marionette client (protocol 2 and 3): a length-prefixed JSON wire protocol.
# ---------------------------------------------------------------------------
class Marionette:
    """Minimal Firefox Marionette client: `length:payload` JSON frames."""

    def __init__(self, port, timeout=45.0):
        deadline = time.time() + timeout
        last = None
        while time.time() < deadline:
            try:
                self.sock = socket.create_connection(("127.0.0.1", port), timeout=20)
                break
            except OSError as exc:  # not listening yet
                last = exc
                time.sleep(0.25)
        else:
            raise RuntimeError(f"Firefox Marionette never listened on :{port}: {last}")
        self.buf = b""
        self.seq = 0
        self.events = []
        self.handshake = self._recv()
        self.protocol = int(self.handshake.get("marionetteProtocol", 3))

    def _recv(self):
        while b":" not in self.buf:
            chunk = self.sock.recv(65536)
            if not chunk:
                raise EOFError("Marionette closed the connection")
            self.buf += chunk
        idx = self.buf.index(b":")
        length = int(self.buf[:idx])
        while len(self.buf) < idx + 1 + length:
            chunk = self.sock.recv(65536)
            if not chunk:
                raise EOFError("Marionette closed mid-message")
            self.buf += chunk
        payload = self.buf[idx + 1:idx + 1 + length]
        self.buf = self.buf[idx + 1 + length:]
        return json.loads(payload.decode())

    def call(self, name, params=None):
        self.seq += 1
        if self.protocol >= 3:
            msg = [0, self.seq, name, params or {}]
        else:
            msg = {"id": self.seq, "name": name, "parameters": params or {}}
        raw = json.dumps(msg).encode()
        self.sock.sendall(str(len(raw)).encode() + b":" + raw)
        while True:
            got = self._recv()
            if isinstance(got, list) and got and got[0] == 2:
                self.events.append(got)  # out-of-band event: keep, never confuse
                continue
            if self.protocol >= 3:
                _, mid, error, result = got
            else:
                mid, error, result = got.get("id"), got.get("error"), got.get("result")
            if mid != self.seq:
                continue
            if error:
                raise RuntimeError(f"Marionette {name} failed: {error}")
            # Marionette wraps script results in {"value": …}; unwrap for callers.
            if isinstance(result, dict) and set(result.keys()) == {"value"}:
                return result["value"]
            return result

    def script(self, script, sandbox=None):
        params = {"script": script, "args": []}
        if sandbox:
            params["sandbox"] = sandbox
        return self.call("WebDriver:ExecuteScript", params)


# The console listener runs in the parent process (`sandbox: system`) and is
# installed *before* the first navigation, so it sees messages the page emits
# while loading — the equivalent of geckodriver's log capture, and the reason a
# boot exception or a CSP violation cannot slip through a race.  console.error
# /warn calls, uncaught content exceptions, failed subresource loads and CSP
# violations all surface here as nsIScriptError/nsIConsoleMessage.
# Never a failure: only the browser's own internals, never the app's.
IGNORED_CONSOLE = ("resource://gre/", "chrome://")

INSTALL_CONSOLE_LISTENER = r"""
if (typeof globalThis.__ttConsole === 'undefined') {
  globalThis.__ttConsole = [];
  var cs = Components.classes["@mozilla.org/consoleservice;1"]
      .getService(Components.interfaces.nsIConsoleService);
  globalThis.__ttListener = {
    observe: function (m) {
      var msg = m.message;
      var src = m.sourceName;
      var line = m.lineNumber;
      if (!msg && m.errorMessage) {
        msg = m.errorMessage.message;
        src = m.errorMessage.sourceName;
        line = m.errorMessage.lineNumber;
      }
      globalThis.__ttConsole.push(String(msg) + " [" + String(src) + ":" + String(line) + "]");
    }
  };
  cs.registerListener(globalThis.__ttListener);
  return "installed";
}
return "already-installed";
"""

# Two sources, deliberately: the live listener catches everything from the
# moment it is registered (before the first navigation), and the service's own
# message array is the same snapshot a `getMessageArray()` probe sees — some
# reports (CSP violations in particular) reach the buffer slightly later than
# the observe() callback, so polling both and de-duplicating is what makes
# "no console errors at all" race-free.
READ_CONSOLE = r"""
var out = {
  listener: globalThis.__ttConsole ? globalThis.__ttConsole.slice() : [],
  all: []
};
var cs = Components.classes["@mozilla.org/consoleservice;1"]
    .getService(Components.interfaces.nsIConsoleService);
var arr = cs.getMessageArray();
for (var i = 0; i < arr.length; i++) {
  var m = arr[i];
  var msg = m.message;
  var src = m.sourceName;
  var line = m.lineNumber;
  if (!msg && m.errorMessage) {
    msg = m.errorMessage.message;
    src = m.errorMessage.sourceName;
    line = m.errorMessage.lineNumber;
  }
  out.all.push(String(msg) + " [" + String(src) + ":" + String(line) + "]");
}
return JSON.stringify(out);
"""


# ---------------------------------------------------------------------------
# Gate
# ---------------------------------------------------------------------------
class Gate:
    def __init__(self, args):
        self.args = args
        self.firefox = args.firefox
        self.binary = args.binary
        # `--dev-web-dir` => the repo's own web/; `--web-dir PATH` => a scratch
        # copy (how the gate self-validates deliberate breakage).
        self.web_dir = args.web_dir or (WEB_DIR if args.dev_web_dir else None)
        self.checks = []
        self.tmp = tempfile.mkdtemp(prefix="tt-browser-smoke-")
        self.data_dir = os.path.join(self.tmp, "data")
        os.makedirs(self.data_dir)
        self.profile = os.path.join(self.tmp, "firefox-profile")
        self.server = None
        self.firefox_proc = None
        self.marionette = None
        self.port = None
        self.console_seen = 0
        # Messages already reported from the console service's own buffer, so a
        # message that arrived during an earlier step is reported once, against
        # the step that produced it, and never re-counted later.
        self.console_reported = set()
        # #232 phase 2e: the last positive/neutral Vanilla button backgrounds
        # sampled during walk_views, compared once the walk has finished.
        self._positive_bg = None
        self._neutral_bg = None
        self.cookie = ""
        self.started = time.time()

    # ------------------------------------------------------------- reporting --
    def check(self, name, ok, detail="", info=""):
        """Record a check. `detail` explains a failure (printed only when the
        check fails); `info` annotates a pass, for the CI log."""
        ok = bool(ok)
        self.checks.append((ok, name, detail))
        line = f"{'PASS' if ok else 'FAIL'}  {name}"
        if not ok and detail:
            line += f"\n        {detail}"
        elif ok and info:
            line += f"  ({info})"
        print(line, flush=True)
        return ok

    @property
    def failed(self):
        return [c for c in self.checks if not c[0]]

    # ---------------------------------------------------------------- HTTP ---
    def http(self, method, path, body=None, cookie=None, extra_headers=None):
        conn = http.client.HTTPConnection("127.0.0.1", self.port, timeout=20)
        headers = {}
        if body is not None:
            headers["content-type"] = "application/json"
        if cookie:
            headers["cookie"] = cookie
        if extra_headers:
            headers.update(extra_headers)
        payload = json.dumps(body).encode() if body is not None else None
        conn.request(method, path, body=payload, headers=headers)
        res = conn.getresponse()
        raw = res.read()
        # Lower-case keys keep lookups case-insensitive whatever the server sends.
        hdrs = {}
        for key, value in res.getheaders():
            hdrs.setdefault(key.lower(), value)
        out = (res.status, hdrs, raw)
        conn.close()
        return out

    # -------------------------------------------------------------- server ---
    def start_server(self):
        if not os.path.exists(self.binary):
            raise RuntimeError(
                f"release binary missing: {self.binary}\n"
                "run `cargo build --locked --release` first"
            )
        self.port = free_port()
        env = dict(os.environ)
        env["TUCANO_DATA_DIR"] = self.data_dir
        env["TUCANO_PORT"] = str(self.port)
        env["TUCANO_STRIPE_FAKE"] = "1"
        env.pop("TUCANO_ENV", None)  # plain HTTP on the loopback: no Secure cookie
        if self.web_dir:
            env["TUCANO_WEB_DIR"] = self.web_dir  # dev mode: disk assets + live reload
        else:
            env.pop("TUCANO_WEB_DIR", None)
        log = open(os.path.join(self.tmp, "server.log"), "wb")
        self.server = subprocess.Popen(
            [self.binary], env=env, stdout=log, stderr=subprocess.STDOUT,
            start_new_session=True,
        )
        deadline = time.time() + 30
        while time.time() < deadline:
            try:
                status, _, _ = self.http("GET", "/healthz")
                if status == 200:
                    return
            except OSError:
                pass
            if self.server.poll() is not None:
                raise RuntimeError(f"server exited early (rc={self.server.returncode})")
            time.sleep(0.2)
        raise RuntimeError("server never became healthy on /healthz")

    # ------------------------------------------------------------- browser ---
    def start_browser(self):
        marionette_port = free_port()
        os.makedirs(self.profile, exist_ok=True)
        with open(os.path.join(self.profile, "prefs.js"), "w", encoding="utf-8") as fh:
            fh.write(f'user_pref("marionette.port", {marionette_port});\n')
            for pref in (
                'user_pref("browser.shell.checkDefaultBrowser", false);',
                'user_pref("datareporting.policy.dataSubmissionEnabled", false);',
                'user_pref("app.update.auto", false);',
                'user_pref("network.proxy.type", 0);',
                'user_pref("browser.startup.page", 0);',
                'user_pref("toolkit.telemetry.enabled", false);',
                # Content sandbox off: CI runs this as root on the self-hosted
                # runners, where the sandbox cannot initialise, and the gate only
                # ever drives our own loopback page.
                'user_pref("security.sandbox.content.level", 0);',
            ):
                fh.write(pref + "\n")
        # HOME is pinned to the scratch dir: Firefox refuses to start as root
        # when $HOME belongs to a regular user, and a systemd service need not
        # export HOME at all. A hermetic HOME removes both hazards.
        env = dict(os.environ)
        env["HOME"] = self.tmp
        env["MOZ_HEADLESS"] = "1"
        self.firefox_log_path = os.path.join(self.tmp, "firefox.log")
        firefox_log = open(self.firefox_log_path, "wb")
        self.firefox_proc = subprocess.Popen(
            [self.firefox, "--headless", "--no-remote", "--marionette",
             "-profile", self.profile, "about:blank"],
            stdout=firefox_log, stderr=subprocess.STDOUT,
            env=env, start_new_session=True,
        )
        try:
            self.marionette = Marionette(marionette_port)
        except RuntimeError as exc:
            raise RuntimeError(f"{exc}\n--- firefox stderr ---\n{self.firefox_tail()}") from exc
        self.marionette.call("WebDriver:NewSession", {"capabilities": {}})
        self.marionette.script(INSTALL_CONSOLE_LISTENER, sandbox="system")

    def firefox_tail(self, lines=25):
        """Last lines of the browser's stderr, for a readable CI failure."""
        try:
            with open(self.firefox_log_path, "rb") as fh:
                text = fh.read().decode("utf-8", "replace")
        except OSError:
            return "(no firefox log)"
        return "\n".join(text.splitlines()[-lines:]) or "(firefox logged nothing)"

    def navigate(self, path):
        self.marionette.call("WebDriver:Navigate", {"url": self.base + path})

    def wait_script(self, script, message, timeout=15.0):
        """Poll a content script until it returns truthy; raise a loud error."""
        deadline = time.time() + timeout
        last = None
        while time.time() < deadline:
            try:
                last = self.marionette.script(
                    "try { return Boolean(%s); } catch (e) { return false; }" % script
                )
                if last:
                    return True
            except RuntimeError as exc:
                last = exc
            time.sleep(0.15)
        raise RuntimeError(f"timed out after {timeout:.0f}s waiting for {message} (last={last!r})")

    # ----------------------------------------------------------- console ----
    def console_since(self, mark):
        raw = self.marionette.script(READ_CONSOLE, sandbox="system")
        data = json.loads(raw) if raw else {}
        listener = data.get("listener") or []
        new = listener[mark:]
        mark = len(listener)
        merged = list(new)
        seen = set(new) | self.console_reported
        for message in data.get("all") or []:
            if message not in seen:
                seen.add(message)
                merged.append(message)
        self.console_reported |= seen
        return merged, mark

    def drain_console(self, name, allow=()):
        """Fail on any console message emitted since the last drain.

        The only exclusion is `IGNORED_CONSOLE`: messages whose source is a
        browser-internal resource (`resource://gre/`, `chrome://`). Those are
        Firefox's own JSWindowActor teardown races — reproducible with any page
        that holds form fields and is navigated programmatically, unrelated to
        the app, and unfixable from this repository. Every app-origin message
        (a module MIME refusal, a boot exception, a CSP violation, a failed
        subresource) carries the app URL and is never excluded.
        """
        new, mark = self.console_since(self.console_seen)
        self.console_seen = mark
        internal = [m for m in new if any(a in m for a in IGNORED_CONSOLE)]
        noise = [
            m for m in new
            if m not in internal and not any(a in m for a in allow)
        ]
        self.check(
            f"no console/page errors: {name}",
            not noise,
            f"{len(noise)} message(s) — the GUI must boot clean:\n        "
            + "\n        ".join(noise),
            info="clean" + (f", {len(internal)} browser-internal message(s) ignored"
                            if internal else ""),
        )
        if self.args.verbose:
            for m in new:
                print(f"        [console] {m}", flush=True)
        return not noise

    # --------------------------------------------------------------- steps --
    def bootstrap(self):
        """Fresh data dir: mint the throwaway admin through the documented
        first-run endpoint, so the shell then shows the ordinary sign-in form."""
        status, _, raw = self.http(
            "POST", "/api/auth/bootstrap",
            {"name": ADMIN_NAME, "email": ADMIN_EMAIL, "password": ADMIN_PASSWORD},
        )
        if status != 201:
            raise RuntimeError(f"/api/auth/bootstrap failed: HTTP {status} {raw[:200]!r}")

    def http_contract(self):
        status, headers, body = self.http("GET", "/")
        ctype = headers.get("content-type", "")
        self.check(
            "HTTP 200 + text/html for /",
            status == 200 and ctype.startswith("text/html"),
            f"status={status} content-type={ctype!r}",
        )
        html = body.decode("utf-8", "replace")
        self.check(
            "the shell declares the ES-module entry point",
            'type="module" src="/app.js"' in html,
            "index.html must load /app.js as a module",
        )

    def module_graph(self):
        """Resolve every specifier the way the browser does: against the URL of
        the module that declares it, and insist on 200 + a JS MIME type (#297)."""
        status, headers, body = self.http("GET", "/app.js")
        self.check(
            "entry module /app.js is served as JavaScript",
            status == 200 and "javascript" in headers.get("content-type", ""),
            f"status={status} content-type={headers.get('content-type')!r}",
        )
        if status != 200:
            return
        specifiers = re.findall(r"^import\s+'([^']+)'", body.decode("utf-8", "replace"), re.M)
        hops = [("/app.js", status, headers.get("content-type", ""))]
        for spec in specifiers:
            url = urljoin("/app.js", spec)
            hop_status, hop_headers, _ = self.http("GET", url)
            hops.append((url, hop_status, hop_headers.get("content-type", "")))
        bad = [h for h in hops if h[1] != 200 or "javascript" not in h[2]]
        self.check(
            "every ES-module hop resolves to 200 + a JS MIME type (#297)",
            not bad,
            "; ".join(f"{u} -> {s} {c!r}" for u, s, c in bad),
            info=f"{len(hops)} hop(s) resolved",
        )
        self.check(
            "the entry module declares its imports",
            len(specifiers) >= 15,
            f"{len(specifiers)} import(s) found in /app.js",
        )

    def boot(self):
        """Load the shell and insist it really booted.

        When the module graph does not execute, report *why* from the console
        drain (a module MIME refusal is the #297 signature) and let the run end
        with recorded assertions rather than an opaque harness error.
        """
        self.navigate("/")
        try:
            self.wait_script("document.readyState === 'complete'",
                             "the shell to load", timeout=20)
        except RuntimeError as exc:
            self.check("shell reached readyState 'complete'", False, str(exc))
            self.drain_console("first load")
            return False
        self.check("shell reached readyState 'complete'",
                   self.marionette.script("return document.readyState === 'complete';"))
        try:
            self.wait_script(
                "!document.getElementById('auth-overlay').hidden",
                "the sign-in overlay (proves the module graph executed)",
                timeout=20,
            )
        except RuntimeError as exc:
            self.check(
                "the module graph executed: auth.js ran and revealed the sign-in view",
                False,
                f"{exc} — the shell's own markup keeps #auth-overlay hidden, so only a "
                "module that executed can reveal it",
            )
            self.drain_console("first load")
            return False
        return True

    def shell_state(self):
        state = self.marionette.script(
            "return JSON.stringify({"
            " readyState: document.readyState,"
            " title: document.title,"
            " authTitle: (document.getElementById('auth-title')||{}).textContent || '',"
            " overlayHidden: document.getElementById('auth-overlay').hidden,"
            " dialogs: document.querySelectorAll('dialog').length,"
            " modules: Array.prototype.map.call("
            "   document.querySelectorAll('script[type=module]'), function (s) { return s.src; }),"
            "});"
        )
        info = json.loads(state)
        self.check("shell reached readyState 'complete'", info["readyState"] == "complete",
                   f"readyState={info['readyState']!r}")
        self.check("expected document title", info["title"] == EXPECTED_TITLE,
                   f"title={info['title']!r} expected {EXPECTED_TITLE!r}")
        self.check(
            "the module graph executed: auth.js ran and revealed the sign-in view",
            info["overlayHidden"] is False and info["authTitle"] == "Sign in",
            f"overlayHidden={info['overlayHidden']} authTitle={info['authTitle']!r}"
            " (the auth overlay is hidden in index.html, so only the module graph can show it)",
        )
        self.check("the shell was loaded as one ES module", len(info["modules"]) == 1
                   and info["modules"][0].endswith("/app.js"), f"modules={info['modules']}")

    def stylesheets(self):
        sheets = self.marionette.script(
            "var out = [];"
            "for (var i = 0; i < document.styleSheets.length; i++) {"
            "  var s = document.styleSheets[i];"
            "  var rules = null;"
            "  try { rules = s.cssRules ? s.cssRules.length : null; } catch (e) { rules = -1; }"
            "  out.push({ href: s.href, rules: rules });"
            "}"
            "return JSON.stringify(out);"
        )
        loaded = json.loads(sheets)
        by_href = {s["href"]: s for s in loaded}
        for expected in EXPECTED_STYLESHEETS:
            hit = by_href.get(self.base + expected)
            self.check(
                f"stylesheet {expected} loaded with rules",
                bool(hit) and bool(hit["rules"]) and hit["rules"] > 0,
                f"missing, or the browser refused it: {hit}",
                info=f"{hit['rules'] if hit else 0} rules applied",
            )

    def dialogs_closed(self):
        result = self.marionette.script(
            "var bad = [];"
            "var total = 0;"
            "var list = document.querySelectorAll('dialog');"
            "for (var i = 0; i < list.length; i++) {"
            "  var d = list[i];"
            "  if (d.hasAttribute('open')) continue;"
            "  total++;"
            "  if (getComputedStyle(d).display !== 'none') {"
            "    bad.push(d.id + ' -> ' + getComputedStyle(d).display);"
            "  }"
            "}"
            "return JSON.stringify({ total: total, bad: bad });"
        )
        info = json.loads(result)
        self.check(
            "the shell ships closed dialogs (guard has something to prove)",
            info["total"] >= 8,
            f"only {info['total']} closed <dialog> elements found",
            info=f"{info['total']} closed dialogs",
        )
        self.check(
            "every closed <dialog> computes display:none (#296)",
            not info["bad"],
            "these closed dialogs are painted: " + ", ".join(info["bad"]),
        )

    def dialog_open_visible(self):
        ok = self.marionette.script(
            "var d = document.getElementById('app-dialog');"
            "d.showModal();"
            "var r = d.getBoundingClientRect();"
            "var res = getComputedStyle(d).display !== 'none' && r.width > 0 && r.height > 0;"
            "return res;"
        )
        self.check("an opened <dialog> is visible and laid out", ok,
                   "showModal() left the dialog non-rendered or zero-sized")
        self.marionette.script(
            "document.getElementById('app-dialog').close(); return true;"
        )

    def theme_toggle(self):
        probe = (
            "var cs = getComputedStyle(document.body);"
            "return document.documentElement.dataset.theme + '|'"
            " + cs.backgroundColor + '|' + cs.color;"
        )
        self.marionette.script(
            "var d = document.querySelector('input[name=colour-mode][value=dark]');"
            "d.checked = true;"
            "d.dispatchEvent(new Event('change', { bubbles: true }));"
            "return true;"
        )
        dark = self.marionette.script(probe)
        self.marionette.script(
            "var l = document.querySelector('input[name=colour-mode][value=light]');"
            "l.checked = true;"
            "l.dispatchEvent(new Event('change', { bubbles: true }));"
            "return true;"
        )
        light = self.marionette.script(probe)
        theme_dark, bg_dark, fg_dark = dark.split("|")
        theme_light, bg_light, fg_light = light.split("|")
        self.check("the theme control sets data-theme", theme_dark == "dark" and theme_light == "light",
                   f"dark={theme_dark!r} light={theme_light!r}")
        self.check(
            "toggling the theme changes the computed colours",
            bg_dark != bg_light and fg_dark != fg_light,
            f"dark bg/fg={bg_dark}/{fg_dark} light bg/fg={bg_light}/{fg_light}",
        )
        # Leave the app in the light theme (its stylesheet default on this host).
        return None

    def sign_in(self):
        self.marionette.script(
            "var e = document.getElementById('auth-email');"
            "e.value = %r;"
            "document.getElementById('auth-password').value = %r;"
            "var f = document.getElementById('auth-form');"
            "f.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true }));"
            "return true;" % (ADMIN_EMAIL, ADMIN_PASSWORD)
        )
        self.wait_script(
            "document.getElementById('version').textContent === 'TucanoTime'",
            "the app to finish startApp() after signing in",
        )
        info = json.loads(self.marionette.script(
            "return JSON.stringify({"
            " overlayHidden: document.getElementById('auth-overlay').hidden,"
            " accountHidden: document.getElementById('account').hidden,"
            " who: document.getElementById('who').textContent,"
            " version: document.getElementById('version').textContent,"
            " dayRows: document.querySelectorAll('#day-table tbody tr').length,"
            "});"
        ))
        self.check(
            "signing in through the real UI boots the app",
            info["overlayHidden"] and not info["accountHidden"]
            and info["who"] == ADMIN_NAME and info["version"] == "TucanoTime",
            f"{info}",
        )

    def seed_fixture(self):
        """One customer + project through the API so the registers have rows."""
        status, headers, _ = self.http(
            "POST", "/api/auth/login",
            {"email": ADMIN_EMAIL, "password": ADMIN_PASSWORD},
        )
        cookie = headers.get("set-cookie", "").split(";")[0]
        self.cookie = cookie
        if not cookie:
            raise RuntimeError("no session cookie from /api/auth/login")
        csrf = {"x-csrf-protection": "1"}
        status, _, raw = self.http(
            "POST", "/api/customers",
            {"name": "ACME", "currency": "EUR", "default_rate_minor": 6000},
            cookie=cookie, extra_headers=csrf,
        )
        if status != 201:
            raise RuntimeError(f"seeding a customer failed: HTTP {status} {raw[:200]!r}")
        customer = json.loads(raw)
        cid = customer.get("id") or customer.get("customer", {}).get("id")
        status, _, raw = self.http(
            "POST", f"/api/customers/{cid}/projects",
            {"code": "SM-1", "name": "Smoke project", "currency": "EUR", "rate_minor": 6000},
            cookie=cookie, extra_headers=csrf,
        )
        if status != 201:
            raise RuntimeError(f"seeding a project failed: HTTP {status} {raw[:200]!r}")

    def walk_views(self):
        for path, tab, panel, probe in VIEWS:
            self.navigate(path)
            self.wait_script(
                "document.readyState === 'complete'"
                " && document.getElementById('version').textContent === 'TucanoTime'",
                f"the app to boot at {path}",
            )
            selected = self.marionette.script(TAB_SELECTED.format(tab=tab))
            rendered = self.marionette.script(RENDERED.format(panel=panel))
            structured = self.marionette.script(
                "try { return Boolean(%s); } catch (e) { return false; }" % probe
            )
            pathname = self.marionette.script("return location.pathname;")
            self.check(
                f"{path} selects its tab and panel",
                selected and pathname.rstrip("/") == path,
                f"tab selected={selected} pathname={pathname!r}",
            )
            self.check(
                f"{path} renders {panel} with layout",
                rendered,
                f"{panel} is hidden or has no layout box",
            )
            self.check(
                f"{path} shows its primary region",
                structured,
                f"structural probe failed for {panel}: {probe}",
            )
            if path == "/timesheets/week":
                align = self.marionette.script(WEEK_COLUMN_ALIGNMENT)
                self.check(
                    "/timesheets/week day headers share their column box (#12)",
                    align == "ok",
                    f"header cells no longer align with the days below them: {align}",
                )
            if path == "/timesheets/calendar":
                align = self.marionette.script(CALENDAR_DAY_ALIGNMENT)
                self.check(
                    "/timesheets/calendar day cells top-align their content (#140)",
                    align == "ok",
                    f"a calendar day cell is not top-aligned: {align}",
                )
            deco = json.loads(self.marionette.script(BUTTON_DECORATION))
            issues = (deco["badSubmit"] + deco["badPrimary"] + deco["badSkin"]
                      + deco["badIcon"] + deco["plus"])
            self.check(
                f"{path} buttons declare the decoration they rely on (#232 phase 2e)",
                not issues,
                "buttons relying on a deleted fallback: " + "; ".join(issues),
            )
            if deco["posBg"]:
                self._positive_bg = deco["posBg"]
            if deco["neuBg"]:
                self._neutral_bg = deco["neuBg"]
            self.drain_console(f"view {path}")

    # ------------------------------------------------ forms (#232 phase 2b) --
    def forms_audit(self):
        """#232 phase 2b: the field/hint/error markup is the official Vanilla
        form vocabulary (`p-form__group` / `p-form__label` / `p-form__control` /
        `p-form-help-text` / `p-form-validation__message` in a
        `p-form-validation is-error` group), and the retired bespoke CSS must
        stop applying. Proved in a real browser: no bespoke class survives, every
        group's label resolves to its own control with an accessible name on
        several views, the app-owned row rhythm / tick-box size / help-text
        override really compute, and a real validation error still surfaces its
        message in the official styling."""
        self.navigate("/settings/security")
        self.wait_script(
            "document.readyState === 'complete'"
            " && document.getElementById('version').textContent === 'TucanoTime'",
            "the app to boot at /settings/security",
        )
        counts = json.loads(self.marionette.script(
            "function n(s) { return document.querySelectorAll(s).length; }"
            "return JSON.stringify({"
            " groups: n('.p-form__group'), labels: n('.p-form__label'),"
            " controls: n('.p-form__control'), help: n('.p-form-help-text'),"
            " messages: n('.p-form-validation__message'),"
            " errorGroups: n('.p-form-validation.is-error'),"
            " field: n('.field'), hint: n('.hint'), formError: n('.form-error')"
            "});"
        ))
        self.check(
            "the bespoke .field/.hint/.form-error markup is gone and the official "
            "form vocabulary is in place (#232 phase 2b)",
            counts["field"] == 0 and counts["hint"] == 0 and counts["formError"] == 0
            and counts["groups"] == 116 and counts["labels"] == 112
            and counts["controls"] == 112 and counts["help"] == 26
            and counts["messages"] == counts["errorGroups"] == 23,
            f"counts={counts}",
            info=f"{counts['groups']} groups, {counts['help']} help texts, "
                 f"{counts['messages']} validation messages",
        )

        pairing = json.loads(self.marionette.script(
            "var bad = []; var groups = document.querySelectorAll('.p-form__group');"
            "for (var i = 0; i < groups.length; i++) {"
            "  var g = groups[i]; var label = null;"
            "  for (var j = 0; j < g.children.length; j++) {"
            "    if (g.children[j].tagName === 'LABEL') { label = g.children[j]; break; }"
            "  }"
            "  if (!label) continue;"  # the 3 grouped fieldsets + the Contacts row
            "  var id = label.getAttribute('for');"
            "  var ctl = id ? document.getElementById(id) : null;"
            "  if (!ctl) { bad.push(String(id) + ': no control'); continue; }"
            "  if (!g.contains(ctl)) bad.push(id + ': control outside its group');"
            "  if (!label.classList.contains('p-form__label')) bad.push(id + ': label class');"
            "  if (!ctl.classList.contains('p-form__control')) bad.push(id + ': control class');"
            "  if (!ctl.labels || ctl.labels.length !== 1) bad.push(id + ': label association');"
            "}"
            "return JSON.stringify(bad);"
        ))
        self.check(
            "every p-form__group pairs its official label with its own control",
            not pairing,
            "broken pairings: " + "; ".join(pairing),
        )

        # Sample labels that are really laid out: the two settings panels that
        # render form rows on load, plus the Day-entry dialog opened for real
        # (#settings-panel-security hides its form when the vault is disabled,
        # so it is deliberately not sampled here).
        problems = []
        sampler = (
            "var ids = %s; var bad = [];"
            "for (var i = 0; i < ids.length; i++) {"
            "  var ctl = document.getElementById(ids[i]);"
            "  if (!ctl) { bad.push(ids[i] + ': missing'); continue; }"
            "  var label = ctl.labels && ctl.labels[0];"
            "  if (!label || !label.textContent.trim()) { bad.push(ids[i] + ': no accessible name'); continue; }"
            "  if (!label.classList.contains('p-form__label')) bad.push(ids[i] + ': label class');"
            "  if (!ctl.classList.contains('p-form__control')) bad.push(ids[i] + ': control class');"
            "  var r = label.getBoundingClientRect();"
            "  if (r.width <= 0 || r.height <= 0) bad.push(ids[i] + ': label not rendered');"
            "}"
            "return JSON.stringify(bad);"
        )
        for path, ids in (("/settings/invoices", ["org-name", "org-accent", "template-subject"]),
                          ("/settings/catalog", ["item-name", "item-desc"])):
            self.navigate(path)
            self.wait_script(
                "document.readyState === 'complete'"
                " && document.getElementById('version').textContent === 'TucanoTime'",
                f"the app to boot at {path}",
            )
            # The router applies the deep link after startApp() set #version, so
            # wait for the sampled control to have a layout box.
            self.wait_script(
                "(function () {"
                f" var n = document.getElementById('{ids[0]}');"
                " return !!(n && n.getBoundingClientRect().height > 0); })()",
                f"the form at {path} to be laid out",
            )
            problems += json.loads(self.marionette.script(sampler % json.dumps(ids)))

        self.navigate("/timesheets/day")
        self.wait_script(
            "document.readyState === 'complete'"
            " && document.getElementById('version').textContent === 'TucanoTime'",
            "the app to boot at /timesheets/day",
        )
        self.marionette.script("document.getElementById('day-add').click(); return true;")
        self.wait_script(
            "(function () { var d = document.getElementById('entry-dialog');"
            " return !!(d && (d.open === true || d.hasAttribute('open'))); })()",
            "the Day-entry dialog to open",
        )
        # The project picker fills on the customer's `change` event (boot.js),
        # so drive that real interaction before sampling the dialog's labels.
        self.marionette.script(
            "var c = document.getElementById('entry-customer');"
            "if (c && c.options.length) { c.selectedIndex = 0;"
            " c.dispatchEvent(new Event('change', { bubbles: true })); }"
            "return true;"
        )
        self.wait_script(
            "(function () { var s = document.getElementById('entry-project');"
            " return !!(s && s.options.length > 0); })()",
            "the entry dialog's project picker to fill",
        )
        problems += json.loads(
            self.marionette.script(sampler % json.dumps(["entry-hours", "entry-note"]))
        )
        self.check(
            "several views and the Day-entry dialog render visible labels that "
            "resolve to their controls",
            not problems,
            "problems: " + "; ".join(problems),
            info="3 views, 7 controls sampled",
        )

        computed = json.loads(self.marionette.script(
            "function cs(node, prop) { return node ? getComputedStyle(node)[prop] : null; }"
            "var row = document.querySelector('#entry-form .p-form__group');"
            "var tick = document.getElementById('entry-billable');"
            "var tickLabel = tick ? tick.closest('label') : null;"
            "var help = document.getElementById('vault-status');"
            "return JSON.stringify({"
            " rowBottom: cs(row, 'marginBottom'),"
            " tickRowBottom: tickLabel ? cs(tickLabel.closest('.p-form__group'), 'marginBottom') : null,"
            " tickWidth: cs(tick, 'width'), tickWeight: cs(tickLabel, 'fontWeight'),"
            " helpTop: cs(help, 'marginTop'), helpSize: cs(help, 'fontSize'),"
            " tip: cs(row, 'display')"
            "});"
        ))
        def px(value):
            return float(str(value).replace("px", "")) if value else -999.0
        self.check(
            "the app-owned form rhythm, tick-box size and help-text margins still "
            "compute as before (#232 phase 2b)",
            abs(px(computed["rowBottom"]) - 10.4) < 0.6          # .65rem row rhythm
            and abs(px(computed["tickRowBottom"]) - 10.4) < 0.6  # tick rows keep it
            and abs(px(computed["tickWidth"]) - 16.0) < 0.6      # 1rem tick box
            and computed["tickWeight"] == "400"                  # the retired no-op
            and px(computed["helpTop"]) >= 0                      # margin: revert
            and abs(px(computed["helpSize"]) - 14.0) < 0.6,      # Vanilla owns .875rem
            f"computed={computed}",
            info=f"row={computed['rowBottom']} tick={computed['tickWidth']} "
                 f"help={computed['helpSize']}/{computed['helpTop']}",
        )

        error = json.loads(self.marionette.script(
            "var proj = document.getElementById('entry-project');"
            "proj.length = 0;"  # force the app's own "Select a project code." path
            "var f = document.getElementById('entry-form');"
            "f.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true }));"
            "var e = document.getElementById('entry-error');"
            "var cs = getComputedStyle(e);"
            "var out = { hidden: e.hidden, text: e.textContent,"
            "  role: e.getAttribute('role'), display: cs.display, color: cs.color,"
            "  wrapper: !!e.closest('.p-form-validation.is-error'),"
            "  height: e.getBoundingClientRect().height };"
            "document.getElementById('entry-dialog').close();"
            "return JSON.stringify(out);"
        ))
        self.check(
            "a real save failure still surfaces its message in the official styling",
            error["hidden"] is False and error["role"] == "alert"
            and error["text"].strip() != "" and error["display"] != "none"
            and error["height"] > 0 and error["wrapper"]
            and error["color"] == "rgb(199, 22, 43)",   # Vanilla's negative token
            f"{error}",
            info=f"message={error['text']!r} colour={error['color']}",
        )

        # #232 phase 2c: the retired `#expense-dialog input { width: 100% }` used
        # to out-specify the tick-box sizing by id count, so #expense-billable
        # painted 100% wide. Prove in a real browser that it now computes to the
        # 1rem box and stays far narrower than its form row (the static guard is
        # gui-test/input-width.test.mjs).
        self.navigate("/expenses")
        self.wait_script(
            "document.readyState === 'complete'"
            " && document.getElementById('version').textContent === 'TucanoTime'",
            "the app to boot at /expenses",
        )
        self.marionette.script("document.getElementById('expense-new').click(); return true;")
        self.wait_script(
            "(function () { var d = document.getElementById('expense-dialog');"
            " return !!(d && (d.open === true || d.hasAttribute('open'))); })()",
            "the expense dialog to open",
        )
        tick = json.loads(self.marionette.script(
            "var t = document.getElementById('expense-billable');"
            "var group = t ? t.closest('.p-form__group') : null;"
            "return JSON.stringify({"
            " width: t ? getComputedStyle(t).width : null,"
            " height: t ? getComputedStyle(t).height : null,"
            " box: t ? t.getBoundingClientRect().width : null,"
            " row: group ? group.getBoundingClientRect().width : null"
            "});"
        ))
        self.check(
            "the #expense-billable tick box keeps its 1rem size, not 100% (#232 phase 2c)",
            abs(px(tick["width"]) - 16.0) < 0.6
            and abs(px(tick["height"]) - 16.0) < 0.6
            and tick["row"] and tick["box"] < tick["row"] * 0.5,
            f"computed={tick}",
            info=f"tick={tick['width']}x{tick['height']} in a {tick['row']}px row",
        )
        self.drain_console("form migration (#232 phase 2b/2c)")

    # ------------------------------------------- sortable register (#232 2d) --
    def invoice_sort_audit(self):
        """#232 phase 2d: the Invoices register is Vanilla 4.59's real sortable
        pattern — `aria-sort` on the `<th>` plus `.p-table__sort-button` (there is
        NO `.p-table--sortable` class). Seed two drafts with different totals
        through the API, then drive the register in a real browser: the active
        column carries `aria-sort` and only that column, clicking a header sorts
        by it, and clicking again flips the direction so the rows reverse."""
        csrf = {"x-csrf-protection": "1"}
        status, _, raw = self.http("GET", "/api/customers", cookie=self.cookie)
        cid = json.loads(raw)["customers"][0]["id"]
        status, _, raw = self.http("GET", f"/api/customers/{cid}/projects", cookie=self.cookie)
        code = json.loads(raw)["projects"][0]["code"]
        for day, hours in (("2026-01-10", 1), ("2026-02-10", 3)):
            status, _, raw = self.http(
                "POST", "/api/entries",
                {"date": day, "customer_id": cid, "project_code": code, "hours": hours},
                cookie=self.cookie, extra_headers=csrf,
            )
            if status != 201:
                raise RuntimeError(f"seeding a sortable entry failed: HTTP {status} {raw[:200]!r}")
        for frm, to in (("2026-01-01", "2026-01-31"), ("2026-02-01", "2026-02-28")):
            status, _, raw = self.http(
                "POST", "/api/invoices",
                {"customer_id": cid, "from": frm, "to": to},
                cookie=self.cookie, extra_headers=csrf,
            )
            if status != 201:
                raise RuntimeError(f"seeding a sortable invoice failed: HTTP {status} {raw[:200]!r}")

        self.navigate("/invoices")
        self.wait_script(
            "document.readyState === 'complete'"
            " && document.getElementById('version').textContent === 'TucanoTime'",
            "the app to boot at /invoices",
        )
        # The register opens on the Open tab, which hides drafts.
        self.marionette.script("document.getElementById('inv-tab-all').click(); return true;")
        self.wait_script(
            "(function () { return document.querySelectorAll('#invoice-table tbody tr').length >= 2; })()",
            "the All tab to list the two seeded drafts",
        )

        def snapshot():
            return json.loads(self.marionette.script(
                "var ths = Array.prototype.slice.call(document.querySelectorAll('#invoice-table thead th'));"
                "return JSON.stringify({"
                " sorts: ths.map(function (th) { return th.getAttribute('aria-sort'); }),"
                " cls: ths.map(function (th) { var b = th.querySelector('button');"
                "   return b ? b.classList.contains('p-table__sort-button') : null; }),"
                " cols: ths.map(function (th) { var b = th.querySelector('button'); return b ? b.getAttribute('data-col') : null; }),"
                " totals: Array.prototype.map.call(document.querySelectorAll('#invoice-table tbody tr'),"
                "   function (r) { return r.children[3].textContent; })"
                "});"
            ))

        def amount(text):
            return float(re.sub(r"[^0-9.]", "", text))

        first = snapshot()
        self.check(
            "the invoice register is Vanilla's real sortable pattern (#232 phase 2d)",
            first["sorts"] == ["ascending", "none", "none", "none", "none", "none", None]
            and first["cols"] == ["number", "customer", "period", "total", "balance", "status", None]
            and first["cls"][:6] == [True, True, True, True, True, True]
            and first["cls"][6] is None,
            f"register={first}",
            info=f"aria-sort={first['sorts']}",
        )
        self.check(
            "the sortable register renders the seeded rows (#232 phase 2d)",
            len(first["totals"]) == 2 and len({amount(t) for t in first["totals"]}) == 2,
            f"totals={first['totals']}",
        )

        btn = "document.querySelector('.p-table__sort-button[data-col=\"total\"]')"
        vals_js = ("var vals = Array.prototype.map.call(document.querySelectorAll"
                   "('#invoice-table tbody tr'), function (r) {"
                   " return parseFloat(r.children[3].textContent.replace(/[^0-9.]/g, '')); });")
        asc_wait = ("(function () { var th = " + btn + ".closest('th');"
                    " if (th.getAttribute('aria-sort') !== 'ascending') return false;"
                    " " + vals_js +
                    " return vals.length >= 2 && vals[0] <= vals[vals.length - 1]; })()")
        desc_wait = ("(function () { var th = " + btn + ".closest('th');"
                     " if (th.getAttribute('aria-sort') !== 'descending') return false;"
                     " " + vals_js +
                     " return vals.length >= 2 && vals[0] > vals[vals.length - 1]; })()")

        self.marionette.script(btn + ".click(); return true;")
        self.wait_script(asc_wait, "the Total header to sort the rows ascending")
        asc = snapshot()
        self.marionette.script(btn + ".click(); return true;")
        self.wait_script(desc_wait, "the Total header to flip the rows to descending")
        desc = snapshot()
        asc_amounts = [amount(t) for t in asc["totals"]]
        desc_amounts = [amount(t) for t in desc["totals"]]
        self.check(
            "clicking a header sorts and toggles direction in a real browser (#232 phase 2d)",
            asc_amounts == sorted(asc_amounts)
            and desc_amounts == sorted(asc_amounts, reverse=True)
            and asc_amounts != desc_amounts
            and asc["sorts"] == ["none", "none", "none", "ascending", "none", "none", None]
            and desc["sorts"] == ["none", "none", "none", "descending", "none", "none", None],
            f"asc={asc['totals']} desc={desc['totals']}",
            info=f"{asc_amounts} -> {desc_amounts}",
        )
        self.drain_console("sortable register (#232 phase 2d)")

    # ------------------------------------------------------------ lifecycle --
    def cleanup(self):
        for proc in (self.firefox_proc, self.server):
            if proc is None:
                continue
            try:
                os.killpg(os.getpgid(proc.pid), signal.SIGTERM)
            except (ProcessLookupError, PermissionError, OSError):
                try:
                    proc.terminate()
                except OSError:
                    pass
        for proc in (self.firefox_proc, self.server):
            if proc is None:
                continue
            try:
                proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                try:
                    os.killpg(os.getpgid(proc.pid), signal.SIGKILL)
                except OSError:
                    try:
                        proc.kill()
                    except OSError:
                        pass
        if self.marionette is not None:
            try:
                self.marionette.sock.close()
            except OSError:
                pass
        if not self.args.keep:
            shutil.rmtree(self.tmp, ignore_errors=True)
        else:
            print(f"[keep] artifacts left in {self.tmp}")

    def screenshot(self):
        path = os.path.join(self.tmp, "failure.png")
        try:
            data = self.marionette.call("WebDriver:TakeScreenshot", {"full": False})
            if isinstance(data, dict):
                data = data.get("value", "")
            if data:
                with open(path, "wb") as fh:
                    fh.write(base64.b64decode(data))
                return path
        except (RuntimeError, EOFError, OSError, ValueError):
            pass
        return None

    def run(self):
        print("=== TucanoTime browser smoke gate (Firefox + Marionette) ===", flush=True)
        print(f"binary   : {self.binary}")
        print(f"assets   : {self.web_dir + ' (disk / TUCANO_WEB_DIR)' if self.web_dir else 'embedded (production)'}")
        print(f"firefox  : {self.firefox}", flush=True)
        self.start_server()
        self.base = f"http://127.0.0.1:{self.port}"
        print(f"server   : {self.base}   data={self.data_dir}", flush=True)
        # HTTP half first: a broken module graph is diagnosed from the served
        # bytes, before the browser is even started.
        self.http_contract()
        self.module_graph()
        self.bootstrap()
        self.start_browser()
        self.marionette.script(INSTALL_CONSOLE_LISTENER, sandbox="system")
        if not self.boot():
            return
        self.shell_state()
        self.drain_console("first load")
        self.stylesheets()
        self.dialogs_closed()
        self.dialog_open_visible()
        self.theme_toggle()
        self.seed_fixture()
        self.sign_in()
        self.drain_console("sign-in")
        self.walk_views()
        self.check(
            "a data-positive button paints the positive skin, distinct from a "
            "neutral Vanilla button (#232 phase 2e)",
            self._positive_bg is not None and self._neutral_bg is not None
            and self._positive_bg != self._neutral_bg,
            f"positive={self._positive_bg!r} neutral={self._neutral_bg!r}",
            info=f"positive bg {self._positive_bg}, neutral bg {self._neutral_bg}",
        )
        self.forms_audit()
        self.invoice_sort_audit()
        if self.web_dir:
            self.dev_live_reload()

    def dev_live_reload(self):
        """Dev-mode contract: the reload snippet is a real asset (CSP-safe) and
        an edit under the web dir actually reloads the page without a manual
        refresh. `--web-dir` is how the gate self-validates a deliberately
        broken copy of the tree without touching the real one."""
        self.navigate("/")
        self.wait_script("document.readyState === 'complete'", "the shell to load")
        info = json.loads(self.marionette.script(
            "return JSON.stringify({"
            " srcs: Array.prototype.map.call(document.scripts, function (s) { return s.src; }),"
            " inline: Array.prototype.filter.call(document.scripts, function (s) { return !s.src; })"
            "   .map(function (s) { return s.textContent.slice(0, 60); }),"
            "});"
        ))
        self.check(
            "dev mode injects the reload snippet as an external asset",
            any(s.endswith("/_dev/reload.js") for s in info["srcs"]),
            f"scripts={info['srcs']}",
        )
        status, headers, body = self.http("GET", "/_dev/reload.js")
        self.check(
            "/_dev/reload.js is served with a JS MIME type",
            status == 200 and headers.get("content-type", "").startswith("text/javascript"),
            f"status={status} content-type={headers.get('content-type')!r}",
        )
        self.check("the reload asset opens the SSE endpoint",
                   b"/_dev/reload" in body and b"EventSource" in body,
                   body[:120].decode("utf-8", "replace"))
        self.drain_console("dev-mode load (the inline snippet must no longer be CSP-blocked)")

        # Bump an mtime under web/ (content untouched, git-clean) and prove the
        # page follows the SSE 'reload' event by itself: the DOM marker we set
        # is gone only if the document was replaced.
        target = os.path.join(self.web_dir, "styles.css")
        original = os.stat(target)
        self.marionette.script("document.body.dataset.smokeMark = '1'; return true;")
        os.utime(target, (original.st_atime, original.st_mtime + 30))
        reloaded = True
        detail = ""
        try:
            self.wait_script(
                "document.readyState === 'complete'"
                " && !document.body.dataset.smokeMark",
                "the SSE reload to replace the document",
                timeout=15,
            )
        except RuntimeError as exc:
            reloaded = False
            detail = str(exc)
        finally:
            os.utime(target, (original.st_atime, original.st_mtime))
        self.check(
            "an edit under the web dir auto-reloads the browser (no manual refresh)",
            reloaded,
            f"{detail} — the SSE /_dev/reload connection did not replace the document",
        )
        self.drain_console("auto-reload")


def urljoin(base, spec):
    """Resolve a module specifier against the URL of the declaring module."""
    if spec.startswith("/"):
        return spec
    directory = base.rsplit("/", 1)[0] or "/"
    parts = (directory.split("/") if directory else []) + spec.split("/")
    out = []
    for part in parts:
        if part in ("", "."):
            continue
        if part == "..":
            if out:
                out.pop()
            continue
        out.append(part)
    return "/" + "/".join(out)


def free_port():
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def resolve_firefox(explicit):
    if explicit:
        if os.path.exists(explicit):
            return explicit
        raise RuntimeError(f"--firefox {explicit!r} does not exist")
    found = shutil.which("firefox")
    if not found:
        raise RuntimeError(
            "Firefox is not on PATH (TUCANO_FIREFOX overrides). This gate drives a "
            "real browser through Marionette; install firefox (e.g. apt-get install "
            "firefox) on the runner — it must NOT be skipped silently."
        )
    return found


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--binary", default=DEFAULT_BINARY,
                        help="path to the release server binary")
    parser.add_argument("--firefox", default=os.environ.get("TUCANO_FIREFOX", ""),
                        help="path to the firefox executable (default: PATH)")
    parser.add_argument("--dev-web-dir", action="store_true",
                        help="serve the assets from web/ (TUCANO_WEB_DIR) and probe "
                             "the dev live-reload path as well")
    parser.add_argument("--web-dir", default="",
                        help="serve the assets from this directory instead (implies "
                             "dev mode / TUCANO_WEB_DIR); useful for self-validation "
                             "against a deliberately broken copy of the tree")
    parser.add_argument("--keep", action="store_true",
                        help="keep the temp data dir/profile for debugging")
    parser.add_argument("--verbose", action="store_true", help="print every console message")
    args = parser.parse_args(argv)

    try:
        args.firefox = resolve_firefox(args.firefox)
    except RuntimeError as exc:
        print(f"ERROR: {exc}", file=sys.stderr)
        return 2

    gate = Gate(args)
    interrupted = {"signal": False}

    def on_signal(signum, _frame):
        interrupted["signal"] = True
        raise KeyboardInterrupt(signum)

    for sig in (signal.SIGINT, signal.SIGTERM):
        signal.signal(sig, on_signal)
    try:
        gate.run()
    except KeyboardInterrupt:
        print("\ninterrupted — cleaning up", file=sys.stderr)
        gate.cleanup()
        return 2
    except Exception as exc:  # harness-level: no Firefox, protocol error, HTTP 5xx…
        print(f"\nHARNESS ERROR: {type(exc).__name__}: {exc}", file=sys.stderr)
        shot = None
        if gate.marionette is not None:
            shot = gate.screenshot()
        if shot:
            print(f"screenshot: {shot}", file=sys.stderr)
        gate.cleanup()
        return 2
    finally:
        gate.cleanup()

    elapsed = time.time() - gate.started
    failed = gate.failed
    print("\n================ browser smoke gate ================")
    if failed:
        print(f"FAILED: {len(failed)} of {len(gate.checks)} checks — {elapsed:.1f}s")
        for _, name, detail in failed:
            print(f"  - {name}" + (f" ({detail})" if detail else ""))
        return 1
    print(f"ALL {len(gate.checks)} BROWSER CHECKS PASSED — {elapsed:.1f}s")
    return 0


if __name__ == "__main__":
    sys.exit(main())
