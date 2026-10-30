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
    let d = r.statusText;
    try { d = (await r.json()).detail || d; } catch {}
    throw new Error(r.status + ': ' + d);
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
  await api('POST', '/admin/register', { email, password });
  toast('registered — an admin must verify this account');
}
