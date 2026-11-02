<!-- SPDX-License-Identifier: AGPL-3.0-only -->
<script>
// First-run guided setup (#39). Three steps — pair a display, make a screen,
// assign a playlist — that check themselves off from REAL derived state only
// (DESIGN.md §10a: never fabricate). The panel retires itself once a device is
// showing a real screen, which is the moment the wall is actually alive.
import { onMount, onDestroy } from 'svelte';
import { api } from './api.js';
import { app, toast } from './store.svelte.js';
import ScreenPreview from './ScreenPreview.svelte';

let devices = $state([]);
let screens = $state([]);
let playlists = $state([]);
let loaded = $state(false);
let timer = null;

async function load() {
  try {
    [devices, screens, playlists] = await Promise.all([
      api('GET', '/admin/devices'),
      api('GET', '/admin/screens'),
      api('GET', '/admin/playlists'),
    ]);
    loaded = true;
  } catch { /* transient; next tick retries */ }
}
onMount(() => {
  load();
  timer = setInterval(() => { if (app.token) load(); }, 10000);
});
onDestroy(() => timer && clearInterval(timer));

// ---- step completion, derived from real fields only ----
const hasDevice = $derived(devices.length > 0);
const hasScreen = $derived(screens.length > 0);
// Step 3 is done when a display points at a playlist that actually has a screen.
const hasPlaylist = $derived(devices.some((d) => d.playlist_id
  && playlists.some((p) => p.id === d.playlist_id && (p.items || []).length > 0)));
const done = $derived(hasDevice && hasScreen && hasPlaylist);

// ---- live preview thumbnails (real renders, placeholder when none yet) ----
const devPreview = $derived(hasDevice ? (devices.find((d) => d.current_screen_id) || devices[0]).current_screen_id || null : null);
const screenPreview = $derived(hasScreen ? screens[0].id : null);
const firstItemScreen = $derived((() => {
  const p = playlists.find((x) => (x.items || []).length > 0);
  return p ? p.items[0].screen_id : null;
})());

const steps = [
  { key: 'devices', n: 1, title: 'Pair a display',
    copy: 'On the display: Settings → Server URL → this address. It appears here on its own.',
    cta: 'Go to Devices' },
  { key: 'screens', n: 2, title: 'Make a screen',
    copy: 'A screen is a small template plus your data. Start from the example and edit it.',
    cta: 'Go to Screens' },
  { key: 'playlists', n: 3, title: 'Assign a playlist',
    copy: 'Put your screen in a playlist and point a display at it. That’s it — the wall is live.',
    cta: 'Go to Playlists' },
];
const complete = { devices: () => hasDevice, screens: () => hasScreen, playlists: () => hasPlaylist };
const thumbs = { devices: () => devPreview, screens: () => screenPreview, playlists: () => firstItemScreen };
const phs = { devices: 'display', screens: 'screen', playlists: 'playlist' };

function go(tab) { app.tab = tab; }
</script>

{#if loaded && !done}
<div class="welcome card">
  <div class="welcome-head">
    <h2>Let’s get your wall live</h2>
    <p class="mut">Three steps, any order. This goes away on its own once a display is showing a screen.</p>
  </div>
  <ol class="welcome-steps">
    {#each steps as s (s.key)}
      {@const isDone = complete[s.key]()}
      <li class="wstep{isDone ? ' done' : ''}">
        <div class="wstep-top">
          <span class="wbadge">{isDone ? '' : s.n}</span>
          <h3>{s.title}</h3>
          {#if isDone}<span class="pill ok">done</span>{/if}
        </div>
        <div class="wstep-thumb">
          <ScreenPreview screenId={thumbs[s.key]()} alt={s.title} ph={phs[s.key]} />
        </div>
        <p class="mut">{s.copy}</p>
        <button class="act" onclick={() => go(s.key)}>{s.cta}</button>
      </li>
    {/each}
  </ol>
</div>
{/if}
