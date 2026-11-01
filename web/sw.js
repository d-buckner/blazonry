/* SPDX-License-Identifier: AGPL-3.0-only */
// mntr admin PWA service worker. Emitted by web-app/build.js with 20261101230922
// replaced by a build stamp, so every deploy produces new bytes and browsers
// pick up the update. Strategy:
//   - app shell ('/', manifest, icons): cache-first (it is one inlined file)
//   - API calls (/admin/*, /api/*): network-only, never cached — device state
//     must never be served stale silently. On network failure the SW tells the
//     client, which shows the offline banner.
//   - everything else: network-first with shell fallback for navigations.

const VERSION = '__BUILD__';
const SHELL = 'mntr-shell-' + VERSION;
const SHELL_URLS = ['/', '/manifest.webmanifest', '/icons/icon-192.png', '/icons/icon-512.png', '/icons/icon.svg'];

self.addEventListener('install', (e) => {
  e.waitUntil(caches.open(SHELL).then((c) => c.addAll(SHELL_URLS)).then(() => self.skipWaiting()));
});

self.addEventListener('activate', (e) => {
  e.waitUntil(
    caches.keys()
      .then((keys) => Promise.all(keys.filter((k) => k.startsWith('mntr-shell-') && k !== SHELL).map((k) => caches.delete(k))))
      .then(() => self.clients.claim())
  );
});

self.addEventListener('message', (e) => {
  if (e.data === 'skip-waiting') self.skipWaiting();
});

async function respond(req) {
  const url = new URL(req.url);
  if (req.method !== 'GET') return fetch(req);

  // API: live or nothing. Report failures so the UI can flag stale state.
  if (url.pathname.startsWith('/admin/') || url.pathname.startsWith('/api/')) {
    try {
      const res = await fetch(req);
      self.clients.matchAll().then((cs) => cs.forEach((c) => c.postMessage('online')));
      return res;
    } catch (err) {
      self.clients.matchAll().then((cs) => cs.forEach((c) => c.postMessage('offline')));
      throw err;
    }
  }

  // Shell assets: cache-first, background refresh on miss.
  if (SHELL_URLS.includes(url.pathname)) {
    const cached = await caches.match(url.pathname);
    if (cached) {
      fetch(req).then((res) => { if (res.ok) caches.open(SHELL).then((c) => c.put(url.pathname, res)); }).catch(() => {});
      return cached;
    }
  }

  // Navigations: network-first, fall back to the cached shell (offline mode).
  if (req.mode === 'navigate') {
    try {
      const res = await fetch(req);
      caches.open(SHELL).then((c) => c.put('/', res.clone()));
      return res;
    } catch (err) {
      const shell = await caches.match('/');
      if (shell) return shell;
      throw err;
    }
  }

  return fetch(req);
}

self.addEventListener('fetch', (e) => {
  e.respondWith(respond(e.request).catch(() => new Response('offline', { status: 503 })));
});
