// api.js — TucanoTime GUI module (ES-module split of web/app.js, #264).
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
  get: (u) => api.request('GET', u),
  post: (u, b) => api.request('POST', u, b),
  put: (u, b) => api.request('PUT', u, b),
  del: (u) => api.request('DELETE', u),
};
