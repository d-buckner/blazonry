<!-- SPDX-License-Identifier: AGPL-3.0-only -->
<script>
import { onMount } from 'svelte';
import { api } from './api.js';
import { app, toast } from './store.svelte.js';
import Dialog from './Dialog.svelte';

let rows = $state([]);
let dlg = $state(null);
let f = $state({ name: '', label: '', width: 800, height: 480, bit_depth: 2 });

async function load() {
  try { rows = await api('GET', '/admin/models'); }
  catch (err) { toast(err.message, true); }
}
onMount(load);
$effect(() => { if (app.tab === 'models') load(); });

function open() {
  f = { name: '', label: '', width: 800, height: 480, bit_depth: 2 };
  dlg?.show();
}
async function save(e) {
  e.preventDefault();
  try {
    await api('POST', '/admin/models', { name: f.name, label: f.label,
      width: +f.width, height: +f.height, bit_depth: +f.bit_depth });
    dlg.close(); load(); toast('saved');
  } catch (err) { toast(err.message, true); }
  return false;
}
async function del(id) {
  if (!confirm('Delete model?')) return;
  try { await api('DELETE', '/admin/models/' + id); load(); } catch (err) { toast(err.message, true); }
}
</script>

<div class="card">
  <div class="rowflex">
    <h3>Device models</h3><span class="sp"></span>
    <button class="pri" onclick={open}>Add model</button>
  </div>
  <table>
    <thead><tr><th>Name</th><th>Label</th><th>Size</th><th>Bit depth</th><th></th></tr></thead>
    <tbody>
    {#each rows as m (m.id)}
      <tr>
        <td class="mono">{m.name}</td>
        <td>{m.label}</td>
        <td>{m.width}×{m.height}</td>
        <td>{m.bit_depth}</td>
        <td class="acts"><button class="act danger" onclick={() => del(m.id)}>Delete</button></td>
      </tr>
    {/each}
    </tbody>
  </table>
</div>

<Dialog bind:this={dlg}>
  <h3 style="margin-bottom:12px">Add model</h3>
  <form onsubmit={save}>
    <div class="row"><label>Name (id)</label><input bind:value={f.name} required></div>
    <div class="row"><label>Label</label><input bind:value={f.label}></div>
    <div class="row"><label>Width</label><input type="number" bind:value={f.width}></div>
    <div class="row"><label>Height</label><input type="number" bind:value={f.height}></div>
    <div class="row"><label>Bit depth</label><input type="number" bind:value={f.bit_depth} min="1" max="2"></div>
    <button class="pri">Save</button>
    <button class="act" type="button" onclick={() => dlg.close()}>Cancel</button>
  </form>
</Dialog>
