<script>
import { onMount } from 'svelte';
import { api } from './api.js';
import { app, ago, toast } from './store.svelte.js';
import Dialog from './Dialog.svelte';

let rows = $state([]);
let dlg = $state(null);
let f = $state({ id: null, name: '', mac: '', playlist_id: '' });

async function load() {
  try { rows = await api('GET', '/admin/devices'); }
  catch (err) { toast(err.message, true); }
}
onMount(load);
$effect(() => {
  if (app.tab !== 'devices') return;
  load();
  const t = setInterval(load, 30000); // keep battery/last-poll fresh
  return () => clearInterval(t);
});

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

<div class="card">
  <div class="rowflex">
    <h3>Devices</h3><span class="sp"></span>
    <button class="pri" onclick={() => open(null)}>Add device</button>
  </div>
  <table>
    <thead><tr><th>Name</th><th>MAC</th><th>Model / FW</th><th>Playlist</th><th>Battery / RSSI</th><th>Last poll</th><th></th></tr></thead>
    <tbody>
    {#each rows as d (d.id)}
      <tr>
        <td>{d.name}</td>
        <td class="mono">{d.mac}</td>
        <td>{d.model || '—'} <span class="mut">{d.firmware_version || ''}</span></td>
        <td>{d.playlist_id ?? ''}{#if d.playlist_id == null}<span class="mut">unassigned</span>{/if}</td>
        <td>{d.battery_voltage ? d.battery_voltage.toFixed(2) + 'V' : '—'} / {d.rssi ?? '—'} dBm</td>
        <td>{ago(d.last_poll_at)}</td>
        <td class="acts">
          <button class="act" onclick={() => open(d.id)}>Edit</button>
          <button class="act danger" onclick={() => del(d.id)}>Delete</button>
        </td>
      </tr>
    {:else}
      <tr><td colspan="7" class="mut">No devices yet — pair one from a TRMNL by pointing it at this server.</td></tr>
    {/each}
    </tbody>
  </table>
</div>

<Dialog bind:this={dlg}>
  <h3 style="margin-bottom:12px">{f.id ? 'Edit device' : 'Add device'}</h3>
  <form onsubmit={save}>
    {#if !f.id}
      <div class="row"><label>MAC</label><input placeholder="AA:BB:CC:DD:EE:FF" bind:value={f.mac} required></div>
    {/if}
    <div class="row"><label>Name</label><input bind:value={f.name}></div>
    <div class="row"><label>Playlist id</label><input type="number" bind:value={f.playlist_id}></div>
    <button class="pri">Save</button>
    <button class="act" type="button" onclick={() => dlg.close()}>Cancel</button>
  </form>
</Dialog>
