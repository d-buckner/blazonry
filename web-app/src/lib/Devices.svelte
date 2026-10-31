<script>
import { onMount } from 'svelte';
import { api } from './api.js';
import { app, ago, toast } from './store.svelte.js';
import Dialog from './Dialog.svelte';
import ScreenPreview from './ScreenPreview.svelte';

let rows = $state([]);
let dlg = $state(null);
let f = $state({ id: null, name: '', mac: '', playlist_id: '' });
let host = $state('');

async function load() {
  try { rows = await api('GET', '/admin/devices'); }
  catch (err) { toast(err.message, true); }
}
onMount(() => { if (typeof location !== 'undefined') host = location.host; });
$effect(() => {
  if (app.tab !== 'devices') return;
  load();
  const t = setInterval(load, 30000); // keep battery / last-poll fresh
  return () => clearInterval(t);
});

// ---- live-state derivation (derived from real fields only) ----
const FLOOR = 1800000; // 30 min
const ageMs = (d) => d.last_poll_at ? Date.now() - new Date(d.last_poll_at.replace(' ', 'T') + 'Z').getTime() : Infinity;
const online = (d) => ageMs(d) < Math.max((d.refresh_interval || 900) * 2000, FLOOR);
const low = (d) => d.battery_voltage != null && d.battery_voltage < 3.3;
const status = (d) => !online(d) ? 'off' : low(d) ? 'low' : 'active';
const statusLabel = (s) => s === 'off' ? 'Offline' : s === 'low' ? 'Low battery' : 'Active';
const batt = (d) => d.battery_voltage != null ? d.battery_voltage.toFixed(2) + ' V' : '—';
const dims = (d) => d.width && d.height ? d.width + '×' + d.height : '—';
const meta = (d) => !online(d) ? 'No poll since ' + ago(d.last_poll_at) + ' · check device'
  : low(d) ? 'Battery ' + batt(d) + ' · charge soon'
  : (d.model || 'device') + ' · updated ' + ago(d.last_poll_at);
const footDim = (d) => low(d) ? 'battery' : !online(d) ? 'offline' : dims(d);

const onlineCount = $derived(rows.filter(online).length);
const attention = $derived(rows.filter((d) => !online(d) || low(d)));
const featured = $derived(rows.find(online) || rows[0] || null);
const lastSync = $derived(rows.reduce((m, d) => (d.last_poll_at && (!m || d.last_poll_at > m) ? d.last_poll_at : m), null));
const firmware = $derived((() => {
  const c = {};
  for (const d of rows) if (d.firmware_version) c[d.firmware_version] = (c[d.firmware_version] || 0) + 1;
  const k = Object.keys(c).sort((a, b) => c[b] - c[a]);
  return k[0] || '—';
})());

// ---- CRUD ----
function open(id) {
  const d = id ? rows.find((r) => r.id === id) : {};
  f = { id: id ?? null, name: d.name ?? '', mac: d.mac ?? '', playlist_id: d.playlist_id ?? '' };
  dlg?.show();
}
async function save(e) {
  e.preventDefault();
  const body = { name: f.name, playlist_id: f.playlist_id === '' ? null : +f.playlist_id };
  try {
    if (f.id) await api('PATCH', '/admin/devices/' + f.id, body);
    else await api('POST', '/admin/devices/import', { devices: [{ mac: f.mac, ...body }] });
    dlg.close(); load(); toast('saved');
  } catch (err) { toast(err.message, true); }
  return false;
}
async function del(id) {
  if (!confirm('Delete this device?')) return;
  try { await api('DELETE', '/admin/devices/' + id); load(); toast('deleted'); }
  catch (err) { toast(err.message, true); }
}
</script>

{#if featured}
  <section class="ov-hero">
    <div class="ov-feature">
      <ScreenPreview screenId={featured.current_screen_id} alt={featured.name} ph={featured.name} />
      <div class="ov-cap"><b>{featured.name}</b> · updated {ago(featured.last_poll_at)}</div>
    </div>
    <aside class="ov-status">
      <h3>Status</h3>
      <div class="ov-srow"><span class="k">Displays</span><span class="v">{onlineCount} <small>/ {rows.length} online</small></span></div>
      <div class="ov-srow"><span class="k">Last sync</span><span class="v">{ago(lastSync)}</span></div>
      <div class="ov-srow"><span class="k">Needs attention</span><span class="v{attention.length ? ' alert' : ''}">{attention.length}</span></div>
      <div class="ov-srow"><span class="k">Firmware</span><span class="v">{firmware}</span></div>
      <div class="ov-srow"><span class="k">Server</span><span class="v">{host || 'LAN'}</span></div>
    </aside>
  </section>
{/if}

<div class="ov-split">
  <section>
    <div class="ov-sec-head">
      <h2>Your displays</h2><span class="rule"></span>
      <button class="ov-btn" onclick={() => open(null)}>+ Add display</button>
    </div>
    <div class="ov-displays">
      {#each rows as d (d.id)}
        <article class="ov-card{featured && d.id === featured.id ? ' sel' : ''}">
          <div class="ov-card-top">
            <span class="id">{String(d.id).padStart(2, '0')}</span>
            <span class="ov-st {status(d)}"><span class="d"></span>{statusLabel(status(d))}</span>
          </div>
          <ScreenPreview screenId={d.current_screen_id} alt={d.name} ph={d.name} />
          <div class="ov-card-body"><h3>{d.name}</h3><p>{meta(d)}</p></div>
          <div class="ov-card-foot">
            <button onclick={() => open(d.id)}>View display</button>
            <span class="dim">{footDim(d)}</span>
          </div>
        </article>
      {:else}
        <div class="ov-empty" style="grid-column:1/-1">No displays yet — pair one from a TRMNL by pointing it at this server.</div>
      {/each}
    </div>
  </section>

  <aside class="ov-attention">
    <div class="head"><h2>Attention</h2><span class="date">{attention.length ? attention.length + ' open' : 'all clear'}</span></div>
    {#if attention.length}
      {#each attention as d (d.id)}
        <div class="ov-attn">
          <span class="mk"></span>
          <span class="who">{d.name}<small>{!online(d) ? 'offline · ' + ago(d.last_poll_at) : 'battery ' + batt(d)}</small></span>
          <button onclick={() => open(d.id)}>View</button>
        </div>
      {/each}
    {:else if rows.length}
      <div class="ov-empty"><span class="ok"><span class="d"></span>All displays healthy</span></div>
    {:else}
      <div class="ov-empty">Nothing to watch yet.</div>
    {/if}
  </aside>
</div>

<Dialog bind:this={dlg}>
  <h3 style="margin-bottom:12px">{f.id ? 'Edit display' : 'Add display'}</h3>
  <form onsubmit={save}>
    {#if !f.id}
      <div class="row"><label>MAC</label><input placeholder="AA:BB:CC:DD:EE:FF" bind:value={f.mac} required></div>
    {/if}
    <div class="row"><label>Name</label><input bind:value={f.name}></div>
    <div class="row"><label>Playlist id</label><input type="number" bind:value={f.playlist_id}></div>
    <button class="pri">Save</button>
    <button class="act" type="button" onclick={() => dlg.close()}>Cancel</button>
    {#if f.id}
      <button class="act danger" type="button" style="float:right" onclick={() => { dlg.close(); del(f.id); }}>Delete</button>
    {/if}
  </form>
</Dialog>
