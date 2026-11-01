<script>
import { onMount } from 'svelte';
import { api } from './api.js';
import { app, toast } from './store.svelte.js';
import Dialog from './Dialog.svelte';

let providers = $state([]);
let extensions = $state([]);
let pdlg = $state(null);
let xdlg = $state(null);

// kind metadata — mirrors the server registries (src/providers, src/extensions)
const PKINDS = {
  caldav: { label: 'Calendar (CalDAV)', shape: 'calendar',
    blurb: 'A CalDAV calendar URL (e.g. Radicale). Events from the next week are pulled automatically.' },
  open_meteo: { label: 'Weather (Open-Meteo)', shape: 'weather',
    blurb: 'Free forecast from Open-Meteo — just a location, no API key needed.' },
};
const XKINDS = {
  calendar: { label: 'Calendar agenda', shape: 'calendar' },
  weather: { label: 'Weather', shape: 'weather' },
};

let pf = $state({ kind: 'caldav', name: '', url: '', user: '', pass: '', lat: '', lon: '', units: 'imperial', refresh: '' });
let xf = $state({ kind: 'calendar', name: '', title: '', days: 3, max_events: 12, time_format: '12h', provider_ids: [], provider_id: '' });

async function load() {
  try {
    [providers, extensions] = await Promise.all([
      api('GET', '/admin/providers'),
      api('GET', '/admin/extensions'),
    ]);
  } catch (err) { toast(err.message, true); }
}
onMount(load);
$effect(() => { if (app.tab === 'connections') load(); });

function providersFor(shape) {
  return providers.filter(p => p.shape === shape);
}

// ---------- providers ----------
function openP() {
  pf = { kind: 'caldav', name: '', url: '', user: '', pass: '', lat: '', lon: '', units: 'imperial', refresh: '' };
  pdlg?.show();
}
async function saveP(e) {
  e.preventDefault();
  const body = { kind: pf.kind, name: pf.name || (pf.kind === 'caldav' ? 'calendar' : 'weather') };
  if (pf.kind === 'caldav') {
    body.config = { url: pf.url };
    if (pf.user) body.config.user = pf.user;
    if (pf.pass) body.config.pass = pf.pass;
  } else {
    body.config = { latitude: +pf.lat, longitude: +pf.lon, units: pf.units };
  }
  if (pf.refresh) body.refresh_secs = +pf.refresh;
  try {
    const out = await api('POST', '/admin/providers', body);
    pdlg.close(); load();
    if (out.first_refresh_error) {
      toast('Saved, but the first fetch failed: ' + out.first_refresh_error, true);
    } else {
      toast(`Connected — ${out.last_item_count ?? 0} item(s) fetched`);
    }
  } catch (err) { toast(err.message, true); }
  return false;
}
async function refreshP(p) {
  try {
    const r = await api('POST', `/admin/providers/${p.id}/refresh`);
    load();
    if (r.ok) toast(r.changed ? `Refreshed — ${r.items} item(s), screen updated` : `Refreshed — ${r.items} item(s), no change`);
    else toast('Refresh failed: ' + r.error, true);
  } catch (err) { toast(err.message, true); }
}
async function delP(p) {
  if (!confirm(`Delete "${p.name}"? Extensions using it will stop updating.`)) return;
  try { await api('DELETE', '/admin/providers/' + p.id); load(); } catch (err) { toast(err.message, true); }
}

// ---------- extensions ----------
function openX() {
  xf = { kind: 'calendar', name: '', title: '', days: 3, max_events: 12, time_format: '12h', provider_ids: [], provider_id: '' };
  xdlg?.show();
}
async function saveX(e) {
  e.preventDefault();
  const body = { kind: xf.kind, name: xf.name || XF_LABEL(xf.kind) };
  const settings = { title: xf.title || undefined };
  if (xf.kind === 'calendar') {
    settings.days = +xf.days; settings.max_events = +xf.max_events; settings.time_format = xf.time_format;
    if (xf.provider_ids.length) settings.provider_ids = xf.provider_ids.map(Number);
  } else {
    settings.days = +xf.days;
    if (xf.provider_id) settings.provider_id = +xf.provider_id;
  }
  body.settings = settings;
  try {
    const out = await api('POST', '/admin/extensions', body);
    xdlg.close(); load();
    toast(`Created — screen #${out.screen_id} added; add it to a playlist to show it`);
  } catch (err) { toast(err.message, true); }
  return false;
}
function XF_LABEL(k) { return k === 'calendar' ? 'Calendar' : 'Weather'; }
async function delX(x) {
  if (!confirm(`Delete "${x.name}"? Its generated screen is removed too.`)) return;
  try { await api('DELETE', '/admin/extensions/' + x.id); load(); } catch (err) { toast(err.message, true); }
}

function ago(iso) {
  if (!iso) return 'never';
  const s = (Date.now() - new Date(iso).getTime()) / 1000;
  if (s < 90) return 'just now';
  if (s < 5400) return Math.round(s / 60) + 'm ago';
  if (s < 172800) return Math.round(s / 3600) + 'h ago';
  return Math.round(s / 86400) + 'd ago';
}
</script>

<div class="card">
  <div class="rowflex">
    <h3>Connections</h3><span class="sp"></span>
    <button class="pri" onclick={openP}>Add source</button>
  </div>
  {#if providers.length === 0}
    <p class="mut">No sources connected yet. A source pulls live information — your calendar or the weather — and feeds the extension screens below.</p>
  {:else}
    <table>
      <thead><tr><th>Name</th><th>Kind</th><th>Last fetch</th><th>Status</th><th></th></tr></thead>
      <tbody>
      {#each providers as p (p.id)}
        <tr>
          <td>{p.name} <span class="mut">#{p.id}</span></td>
          <td>{PKINDS[p.kind]?.label ?? p.kind}</td>
          <td class="mut">{ago(p.last_ok_at)}</td>
          <td>{#if p.last_error}<span class="pill warn">{p.last_error}</span>{:else}<span class="pill ok">ok</span>{/if}</td>
          <td class="acts">
            <button class="act" onclick={() => refreshP(p)}>Refresh now</button>
            <button class="act danger" onclick={() => delP(p)}>Delete</button>
          </td>
        </tr>
      {/each}
      </tbody>
    </table>
  {/if}
</div>

<div class="card">
  <div class="rowflex">
    <h3>Extensions</h3><span class="sp"></span>
    <button class="pri" onclick={openX}>Add extension</button>
  </div>
  {#if extensions.length === 0}
    <p class="mut">Extensions turn your data into ready-made screens — a calendar agenda or a weather card. Add one, then drop its screen into a playlist.</p>
  {:else}
    <table>
      <thead><tr><th>Name</th><th>Kind</th><th>Screen</th><th></th></tr></thead>
      <tbody>
      {#each extensions as x (x.id)}
        <tr>
          <td>{x.name} <span class="mut">#{x.id}</span></td>
          <td>{XKINDS[x.kind]?.label ?? x.kind}</td>
          <td class="mut">#{x.screen_id}</td>
          <td class="acts"><button class="act danger" onclick={() => delX(x)}>Delete</button></td>
        </tr>
      {/each}
      </tbody>
    </table>
  {/if}
</div>

<Dialog bind:this={pdlg}>
  <h3 style="margin-bottom:12px">Add connection</h3>
  <form onsubmit={saveP}>
    <div class="row"><label>Kind</label>
      <select bind:value={pf.kind} class="ctl">
        {#each Object.entries(PKINDS) as [k, v] (k)}<option value={k}>{v.label}</option>{/each}
      </select>
    </div>
    <p class="mut blurb">{PKINDS[pf.kind].blurb}</p>
    <div class="row"><label>Name</label><input bind:value={pf.name} placeholder={pf.kind === 'caldav' ? 'Family calendar' : 'Home weather'}></div>
    {#if pf.kind === 'caldav'}
      <div class="row"><label>Calendar URL</label><input bind:value={pf.url} required placeholder="https://caldav.example.com/user/calendar/"></div>
      <div class="row"><label>User (optional)</label><input bind:value={pf.user} autocomplete="off"></div>
      <div class="row"><label>Password (optional)</label><input type="password" bind:value={pf.pass} autocomplete="off"></div>
    {:else}
      <div class="row"><label>Latitude</label><input type="number" step="any" min="-90" max="90" bind:value={pf.lat} required placeholder="47.75"></div>
      <div class="row"><label>Longitude</label><input type="number" step="any" min="-180" max="180" bind:value={pf.lon} required placeholder="-122.34"></div>
      <div class="row"><label>Units</label><select bind:value={pf.units} class="ctl"><option value="imperial">Imperial (°F)</option><option value="metric">Metric (°C)</option></select></div>
    {/if}
    <div class="row"><label>Refresh (secs)</label><input type="number" min="60" bind:value={pf.refresh} placeholder="900"></div>
    <div class="dlg-actions">
      <button class="pri">Connect</button>
      <button class="act" type="button" onclick={() => pdlg.close()}>Cancel</button>
    </div>
  </form>
</Dialog>

<Dialog bind:this={xdlg}>
  <h3 style="margin-bottom:12px">Add extension</h3>
  <form onsubmit={saveX}>
    <div class="row"><label>Kind</label>
      <select bind:value={xf.kind} class="ctl">
        {#each Object.entries(XKINDS) as [k, v] (k)}<option value={k}>{v.label}</option>{/each}
      </select>
    </div>
    <div class="row"><label>Name</label><input bind:value={xf.name} placeholder={XF_LABEL(xf.kind)}></div>
    <div class="row"><label>Screen title</label><input bind:value={xf.title} placeholder={XF_LABEL(xf.kind)}></div>
    {#if xf.kind === 'calendar'}
      {@const cands = providersFor('calendar')}
      {#if cands.length === 0}
        <p class="mut" style="margin:0 0 10px">No calendar sources yet — add one above, or this extension will show an empty agenda until you do.</p>
      {:else}
        <div class="row"><label>Sources</label>
          <select multiple size="3" bind:value={xf.provider_ids} class="ctl">
            {#each cands as p (p.id)}<option value={p.id}>{p.name}</option>{/each}
          </select>
        </div>
        <p class="mut blurb">Nothing selected = all calendar sources.</p>
      {/if}
      <div class="row"><label>Days ahead</label><input type="number" min="1" max="8" bind:value={xf.days}></div>
      <div class="row"><label>Max events</label><input type="number" min="1" max="50" bind:value={xf.max_events}></div>
      <div class="row"><label>Time format</label><select bind:value={xf.time_format} class="ctl"><option value="12h">12-hour</option><option value="24h">24-hour</option></select></div>
    {:else}
      {@const cands = providersFor('weather')}
      {#if cands.length === 0}
        <p class="mut" style="margin:0 0 10px">No weather source yet — add one above first.</p>
      {:else}
        <div class="row"><label>Source</label>
          <select bind:value={xf.provider_id} class="ctl">
            <option value="">Default</option>
            {#each cands as p (p.id)}<option value={p.id}>{p.name}</option>{/each}
          </select>
        </div>
      {/if}
      <div class="row"><label>Forecast days</label><input type="number" min="0" max="4" bind:value={xf.days}></div>
    {/if}
    <div class="dlg-actions">
      <button class="pri">Create</button>
      <button class="act" type="button" onclick={() => xdlg.close()}>Cancel</button>
    </div>
  </form>
</Dialog>
