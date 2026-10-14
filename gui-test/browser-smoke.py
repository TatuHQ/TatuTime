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
ADMIN_PASSWORD = "smoke-pass-123456"

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
    ("/timesheets/calendar", "tab-timesheet", "panel-calendar",
     "(() => { const t = document.getElementById('cal-grid');"
     " return !!t && !!t.querySelector('tbody'); })()"),
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
            self.drain_console(f"view {path}")

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
