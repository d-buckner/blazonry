<script>
import { onMount } from 'svelte';
import { api } from './api.js';
import { app, toast } from './store.svelte.js';
import Dialog from './Dialog.svelte';

let pls = $state([]);
let screens = $state([]);
let dlg = $state(null);
let f = $state({ id: null, name: '' });
// selected "add screen" option per playlist id
let sel = $state({});

async function load() {
  try {
    [pls, screens] = await Promise.all([api('GET', '/admin/playlists'), api('GET', '/admin/screens')]);
  } catch (err) { toast(err.message, true); }
}
onMount(load);
$effect(() => { if (app.tab === 'playlists') load(); });

function open(id) {
  const p = id ? pls.find((x) => x.id === id) : {};
  f = { id: id ?? null, name: p.name ?? '' };
  dlg?.show();
}
async function save(e) {
  e.preventDefault();
  try {
    if (f.id) await api('PATCH', '/admin/playlists/' + f.id, { name: f.name });
    else await api('POST', '/admin/playlists', { name: f.name, screen_ids: [] });
    dlg.close(); load(); toast('saved');
  } catch (err) { toast(err.message, true); }
  return false;
}
async function del(id) {
  if (!confirm('Delete playlist (devices will be unassigned)?')) return;
  try { await api('DELETE', '/admin/playlists/' + id); load(); toast('deleted'); }
  catch (err) { toast(err.message, true); }
}
async function addItem(p) {
  try { await api('POST', `/admin/playlists/${p.id}/items`, { screen_id: +sel[p.id] }); load(); }
  catch (err) { toast(err.message, true); }
}
async function toggleItem(p, it) {
  try { await api('PATCH', `/admin/playlists/${p.id}/items/${it.screen_id}`, { visible: !it.visible }); load(); }
  catch (err) { toast(err.message, true); }
}
async function delItem(p, it) {
  try { await api('DELETE', `/admin/playlists/${p.id}/items/${it.screen_id}`); load(); }
  catch (err) { toast(err.message, true); }
}
</script>

<div class="card">
  <div class="rowflex">
    <h3>Playlists</h3><span class="sp"></span>
    <button class="pri" onclick={() => open(null)}>New playlist</button>
  </div>
  {#each pls as p (p.id)}
    <div class="pl-card">
      <div class="pl-head">
        <b>{p.name}</b> <span class="mut">#{p.id}</span><span class="sp"></span>
        <button class="act" onclick={() => open(p.id)}>Rename</button>
        <button class="act danger" onclick={() => del(p.id)}>Delete</button>
      </div>
      <table style="margin-top:8px">
        <thead><tr><th>#</th><th>Screen</th><th>Visible</th><th></th></tr></thead>
        <tbody>
        {#each p.items || [] as i (i.screen_id)}
          <tr>
            <td>{i.position}</td>
            <td>{i.name} <span class="mut">#{i.screen_id}</span></td>
            <td><span class="pill {i.visible ? 'ok' : 'warn'}">{i.visible ? 'on' : 'off'}</span></td>
            <td class="acts">
              <button class="act" onclick={() => toggleItem(p, i)}>{i.visible ? 'Hide' : 'Show'}</button>
              <button class="act danger" onclick={() => delItem(p, i)}>Remove</button>
            </td>
          </tr>
        {:else}
          <tr><td colspan="4" class="mut">Empty — add a screen below.</td></tr>
        {/each}
        </tbody>
      </table>
      <div style="margin-top:8px;display:flex;gap:8px">
        <select style="max-width:260px" bind:value={sel[p.id]}>
          {#each screens as s (s.id)}<option value={s.id}>{s.name}</option>{/each}
        </select>
        <button class="act" onclick={() => addItem(p)}>Add screen</button>
      </div>
    </div>
  {:else}
    <p class="mut">No playlists yet.</p>
  {/each}
</div>

<Dialog bind:this={dlg}>
  <h3 style="margin-bottom:12px">{f.id ? 'Rename playlist' : 'New playlist'}</h3>
  <form onsubmit={save}>
    <div class="row"><label>Name</label><input bind:value={f.name} required></div>
    <button class="pri">Save</button>
    <button class="act" type="button" onclick={() => dlg.close()}>Cancel</button>
  </form>
</Dialog>
