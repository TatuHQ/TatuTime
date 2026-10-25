// api.js — TatuTime GUI module (ES-module split of web/app.js, #264).
// Moved from the original monolithic app.js; behaviour is unchanged.


// Every JSON endpoint lives under /api (page routing); the top-level paths are
// GUI page routes (see the URL router near the boot section). One base keeps
// all callers unchanged and stops the API shadowing page paths like /customers.
export const API_BASE = '/api';

/// The `Error` a failed request throws. The method, the path and what the
/// endpoint does accept are all worth naming: "HTTP 405" alone leaves a developer
/// guessing between a wrong verb, a stale server and a proxy in the middle. The
/// server's own message wins when it sent one (the app's one error shape).
export function requestFailure(method, url, res, data) {
  const allow = res.headers.get('allow');
  const detail = allow
    ? ` (this endpoint accepts ${allow.split(',').map((verb) => verb.trim()).join(', ')})`
    : '';
  const err = new Error(
    data?.error?.message || `${method} ${API_BASE}${url} failed: HTTP ${res.status}${detail}`,
  );
  err.status = res.status;
  err.payload = data;
  return err;
}

export const api = {
  async request(method, url, body) {
    const opts = { method, headers: {}, credentials: 'same-origin' };
    // CSRF guard (#46): required on all mutating requests.
    opts.headers['X-CSRF-Protection'] = '1';
    if (body !== undefined) {
      opts.headers['Content-Type'] = 'application/json';
      opts.body = JSON.stringify(body);
    }
    const res = await fetch(API_BASE + url, opts);
    if (res.status === 204) return null;
    const text = await res.text();
    let data = null;
    if (text) {
      try {
        data = JSON.parse(text);
      } catch {
        // Non-JSON body (e.g. a proxy 502 HTML page): map to the status like
        // getBlob does instead of leaking a raw SyntaxError (#264).
        data = null;
      }
    }
    if (!res.ok) throw requestFailure(method, url, res, data);
    return data;
  },
  // Fetch a binary body with the session cookie intact (#113). A bare
  // <a href> would also send the cookie, but we need to inspect the status
  // and map error JSON, so this mirrors request() for the non-JSON case.
  async getBlob(url) {
    const res = await fetch(API_BASE + url, { method: 'GET', headers: {}, credentials: 'same-origin' });
    if (!res.ok) {
      const text = await res.text().catch(() => '');
      let data = null;
      try {
        data = JSON.parse(text);
      } catch {
        /* body was not the JSON error shape */
      }
      throw requestFailure('GET', url, res, data);
    }
    return res.blob();
  },
  // POST a candidate payload and read a binary body (#new live template
  // preview). Same session cookie + CSRF header as `request`; the difference is
  // the reply: `{ blob, etag, notModified }` instead of parsed JSON. `304` is a
  // success here, not a redirect to follow: the caller already has the bytes
  // `ifNoneMatch` names, so the API reports `notModified` and hands back no
  // body. A non-OK status is still mapped through the one error shape, so a 422
  // arrives with its `fields` intact.
  async requestBlob(method, url, body, { ifNoneMatch = null } = {}) {
    const opts = { method, headers: {}, credentials: 'same-origin' };
    opts.headers['X-CSRF-Protection'] = '1';
    if (ifNoneMatch) opts.headers['If-None-Match'] = ifNoneMatch;
    if (body !== undefined) {
      opts.headers['Content-Type'] = 'application/json';
      opts.body = JSON.stringify(body);
    }
    const res = await fetch(API_BASE + url, opts);
    if (res.status === 304) {
      return { notModified: true, etag: ifNoneMatch, blob: null, status: 304 };
    }
    if (!res.ok) {
      const text = await res.text().catch(() => '');
      let data = null;
      try {
        data = JSON.parse(text);
      } catch {
        /* body was not the JSON error shape */
      }
      throw requestFailure(method, url, res, data);
    }
    return {
      notModified: false,
      etag: res.headers.get('etag'),
      blob: await res.blob(),
      status: res.status,
    };
  },
  // Raw-body PUT for the Harvest import sources (#402/#403): CSV/ZIP bytes,
  // never base64 JSON. Same cookie + CSRF discipline and error mapping.
  async upload(url, body, contentType) {
    const res = await fetch(API_BASE + url, {
      method: 'PUT',
      headers: { 'X-CSRF-Protection': '1', 'Content-Type': contentType },
      credentials: 'same-origin',
      body,
    });
    const text = await res.text();
    let data = null;
    if (text) {
      try {
        data = JSON.parse(text);
      } catch {
        data = null;
      }
    }
    if (!res.ok) throw requestFailure('PUT', url, res, data);
    return data;
  },
  get: (u) => api.request('GET', u),
  post: (u, b) => api.request('POST', u, b),
  put: (u, b) => api.request('PUT', u, b),
  del: (u) => api.request('DELETE', u),
};

/// How many pages a single call to `fetchAll` will walk. A page is bounded by
/// the server (200 rows by default, 1000 at most), so this is a runaway guard,
/// not a truncation policy: reaching it is reported, never swallowed.
export const MAX_PAGES = 50;

/// Every row a list endpoint has for a query, in its documented order (#334).
///
/// The two list endpoints page: a response carries at most `limit` rows and
/// answers `next_cursor` — null when that page ended the matching set, or an
/// opaque cursor when it did not. A view that read only the first page would
/// silently drop the tail, which is the failure pagination exists to prevent, so
/// every view that needs *all* the rows it asked for walks the cursors here.
///
/// The first request is the caller's own URL, unchanged: the bound is the
/// server's default, not something each view has to opt into, and the cursor is
/// only ever sent back verbatim — the app never parses one.
///
/// `truncated` is the one case where rows are left behind (the walk hit
/// [`MAX_PAGES`]). Callers must say so; it means the collection outgrew the
/// harness, not that the view may quietly show less.
export async function fetchAll(path, key) {
  const rows = [];
  let cursor = null;
  for (let page = 0; page < MAX_PAGES; page += 1) {
    const url = cursor
      ? `${path}${path.includes('?') ? '&' : '?'}cursor=${encodeURIComponent(cursor)}`
      : path;
    const data = await api.get(url);
    rows.push(...(data[key] || []));
    cursor = data.next_cursor || null;
    if (!cursor) return { rows, truncated: false };
  }
  return { rows, truncated: true };
}
