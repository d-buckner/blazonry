/* SPDX-License-Identifier: AGPL-3.0-only */
// Runes-based shared state. token starts empty so the first client render
// matches the SSR'd (logged-out) HTML; onMount fills it in afterwards.
export const app = $state({
  token: '',
  email: '',
  role: '',
  tab: 'devices',
  booted: false,
  online: true,        // false when the SW reports API unreachable
  updateReady: false,  // a new service worker is waiting
});

export const toasts = $state([]);
let toastSeq = 0;
export function toast(msg, err) {
  const id = ++toastSeq;
  toasts.push({ id, msg, err: !!err });
  setTimeout(() => {
    const i = toasts.findIndex(t => t.id === id);
    if (i >= 0) toasts.splice(i, 1);
  }, 4000);
}

export function ago(ts) {
  if (!ts) return '—';
  const s = (Date.now() - new Date(ts.replace(' ', 'T') + 'Z').getTime()) / 1000;
  if (s < 90) return 'just now';
  if (s < 5400) return Math.round(s / 60) + 'm ago';
  if (s < 172800) return Math.round(s / 3600) + 'h ago';
  return Math.round(s / 86400) + 'd ago';
}
