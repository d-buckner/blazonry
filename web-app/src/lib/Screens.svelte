<!-- SPDX-License-Identifier: AGPL-3.0-only -->
<script>
import { onMount } from 'svelte';
import { api } from './api.js';
import { app, ago, toast } from './store.svelte.js';
import Dialog from './Dialog.svelte';
import PreviewImg from './PreviewImg.svelte';

const EXAMPLE = JSON.stringify({ name: "hello", parts: [
  { type: "text", text: "{{ title }}", style: { font_px: 42, bold: true }, padding: [40, 40, 8, 40] },
  { type: "keyed_value", key: "Message", value: "{{ body }}", padding: [20, 40], style: { font_px: 26 } }
]}, null, 2);

let rows = $state([]);
let dlg = $state(null);
let f = $state({ id: null, name: '', template: EXAMPLE, vars: '{}' });
let showPreview = $state(false);
let previewUrl = $state('');
let previewErr = $state('');

async function load() {
  try { rows = await api('GET', '/admin/screens'); }
  catch (err) { toast(err.message, true); }
}
onMount(load);
$effect(() => { if (app.tab === 'screens') load(); });

function open(id) {
  const s = id ? rows.find((r) => r.id === id) : {};
  f = { id: id ?? null, name: s.name ?? '', template: s.template ?? EXAMPLE, vars: s.vars ?? '{}' };
  showPreview = false; previewUrl = ''; previewErr = '';
  dlg?.show();
}
async function save(e) {
  e.preventDefault();
  const body = { name: f.name, template: f.template, vars: f.vars };
  try {
    if (f.id) await api('PATCH', '/admin/screens/' + f.id, body);
    else await api('POST', '/admin/screens', body);
    dlg.close(); load(); toast('saved');
  } catch (err) { toast(err.message, true); }
  return false;
}
// Real draft preview: render the dialog's current template+vars without
// saving (#21 — the old button showed a static logo).
async function preview() {
  showPreview = true; previewErr = ''; previewUrl = '';
  try {
    const r = await fetch('/admin/screens/preview', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json', Authorization: 'Bearer ' + app.token },
      body: JSON.stringify({ template: f.template, vars: f.vars }),
    });
    if (!r.ok) throw new Error(await r.text() || r.statusText);
    previewUrl = URL.createObjectURL(await r.blob());
  } catch (err) { previewErr = err.message; }
}
async function del(id) {
  if (!confirm('Delete this screen (removed from all playlists)?')) return;
  try { await api('DELETE', '/admin/screens/' + id); load(); toast('deleted'); }
  catch (err) { toast(err.message, true); }
}
</script>

<div class="card">
  <div class="rowflex">
    <h3>Screens</h3><span class="sp"></span>
    <button class="pri" onclick={() => open(null)}>New screen</button>
  </div>
  <table>
    <thead><tr><th></th><th>Name</th><th>Updated</th><th></th></tr></thead>
    <tbody>
    {#each rows as s (s.id)}
      <tr>
        <td><PreviewImg id={s.id} /></td>
        <td><b>{s.name}</b></td>
        <td>{ago(s.updated_at)}</td>
        <td class="acts">
          <button class="act" onclick={() => open(s.id)}>Edit</button>
          <button class="act danger" onclick={() => del(s.id)}>Delete</button>
        </td>
      </tr>
    {:else}
      <tr><td colspan="4" class="mut">No screens yet.</td></tr>
    {/each}
    </tbody>
  </table>
</div>

<Dialog bind:this={dlg}>
  <h3 style="margin-bottom:12px">{f.id ? 'Edit screen' : 'New screen'}</h3>
  <form onsubmit={save}>
    <div class="row"><label>Name</label><input bind:value={f.name} required></div>
    <div class="row"><label>Template (JSON)</label><textarea bind:value={f.template}></textarea></div>
    <div class="row"><label>Variables (JSON)</label><textarea style="min-height:80px" bind:value={f.vars}></textarea></div>
    <button class="pri">Save</button>
    <button class="act" type="button" onclick={preview}>Preview</button>
    <button class="act" type="button" onclick={() => dlg.close()}>Cancel</button>
  </form>
  {#if showPreview}
    <div style="margin-top:12px">
      {#if previewErr}<p class="mut" style="color:var(--red)">{previewErr}</p>
      {:else if previewUrl}<img class="prev" style="max-width:400px" src={previewUrl} alt="Screen preview">
      {:else}<p class="mut">Rendering…</p>{/if}
    </div>
  {/if}
</Dialog>
