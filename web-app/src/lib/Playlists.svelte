<!-- SPDX-License-Identifier: AGPL-3.0-only -->
<script>
import { onMount } from 'svelte';
import { api } from './api.js';
import { app, toast } from './store.svelte.js';
import Dialog from './Dialog.svelte';
import Empty from './Empty.svelte';

let pls = $state([]);
let screens = $state([]);
let dlg = $state(null);
let f = $state({ id: null, name: '' });
// selected "add screen" option per playlist id
let sel = $state({});

async function load() {
  try {
    [pls, screens] = await Promise.all([api('GET', '/admin/playlists'), api('GET', '/admin/screens')]);
    // json_group_array does not guarantee ORDER BY order — sort client-side
    // so display order always equals position order (#22).
    for (const p of pls) p.items = (p.items || []).sort((a, b) => a.position - b.position);
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
// Reorder via the full-replace endpoint: positions are reassigned 0..n in
// order, which also heals any legacy duplicate-position rows (#22).
async function move(p, idx, dir) {
  const ids = p.items.map((i) => i.screen_id);
  const j = idx + dir;
  if (j < 0 || j >= ids.length) return;
  [ids[idx], ids[j]] = [ids[j], ids[idx]];
  try { await api('PATCH', '/admin/playlists/' + p.id, { screen_ids: ids }); load(); }
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
      {#if p.items?.length}
      <table style="margin-top:8px">
        <thead><tr><th>#</th><th>Screen</th><th>Visible</th><th></th></tr></thead>
        <tbody>
        {#each p.items as i, idx (i.screen_id)}
          <tr>
            <td>{idx + 1}</td>
            <td>{i.name} <span class="mut">#{i.screen_id}</span></td>
            <td><span class="pill {i.visible ? 'ok' : 'warn'}">{i.visible ? 'on' : 'off'}</span></td>
            <td class="acts">
              <button class="act" disabled={idx === 0} onclick={() => move(p, idx, -1)} title="Move up">↑</button>
              <button class="act" disabled={idx === p.items.length - 1} onclick={() => move(p, idx, 1)} title="Move down">↓</button>
              <button class="act" onclick={() => toggleItem(p, i)}>{i.visible ? 'Hide' : 'Show'}</button>
              <button class="act danger" onclick={() => delItem(p, i)}>Remove</button>
            </td>
          </tr>
        {/each}
        </tbody>
      </table>
      {:else}
        <Empty compact title="Nothing queued"
          blurb="Add a screen below and it joins the rotation." />
      {/if}
      <div style="margin-top:8px;display:flex;gap:8px">
        <select style="max-width:260px" bind:value={sel[p.id]}>
          {#each screens as s (s.id)}<option value={s.id}>{s.name}</option>{/each}
        </select>
        <button class="act" onclick={() => addItem(p)}>Add screen</button>
      </div>
    </div>
  {:else}
    <Empty title="No playlists yet"
      blurb="A playlist is the rotation one display cycles through. Make one, add screens to it, then point a display at it."
      action="New playlist" onaction={() => open(null)} />
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
