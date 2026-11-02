/* SPDX-License-Identifier: AGPL-3.0-only */
import { app, toast } from './store.svelte.js';

export async function api(method, path, body) {
  const opts = { method, headers: {} };
  if (app.token) opts.headers.Authorization = 'Bearer ' + app.token;
  if (body !== undefined) {
    opts.headers['Content-Type'] = 'application/json';
    opts.body = JSON.stringify(body);
  }
  const r = await fetch(path, opts);
  if (r.status === 401 && app.token) { logout(); throw new Error('signed out'); }
  if (!r.ok) {
    // Server errors are plain text (axum String) or JSON {detail}; prefer the
    // human message over the status line — users never need "403: Forbidden".
    let d = '';
    const body = await r.text().catch(() => '');
    try { d = JSON.parse(body).detail || ''; } catch { d = body; }
    if (r.status === 403 && /verification/i.test(d))
      d = 'Your account is waiting for an admin to approve it.';
    throw new Error(d || r.status + ': ' + r.statusText);
  }
  return r.status === 204 ? null : r.json();
}

// Authed image fetch -> object URL (bearer token can't ride on <img src>)
export async function authedBlobUrl(path) {
  const r = await fetch(path, { headers: { Authorization: 'Bearer ' + app.token } });
  if (!r.ok) throw new Error(r.statusText);
  return URL.createObjectURL(await r.blob());
}

export function logout() {
  app.token = ''; app.email = ''; app.role = '';
  sessionStorage.removeItem('tr-token');
}

export async function doLogin(email, password) {
  const r = await api('POST', '/admin/login', { email, password });
  app.token = r.token; app.email = r.email || email; app.role = r.role;
  sessionStorage.setItem('tr-token', r.token);
  try {
    const me = await api('GET', '/admin/me');
    app.email = me.email; app.role = me.role;
  } catch {}
}

export async function doRegister(email, password) {
  const r = await api('POST', '/admin/register', { email, password });
  // The first account IS the admin — it gets a session token back, so
  // sign them straight in instead of bouncing to the login form (#20/#23).
  if (r.role === 'admin') {
    sessionStorage.setItem('tr-token', r.token);
    app.token = r.token; app.email = email; app.role = 'admin';
    toast("You're the admin — welcome to blazonry.");
    return;
  }
  toast('Account created — an admin needs to approve it before you can sign in.');
}
