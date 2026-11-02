<!-- SPDX-License-Identifier: AGPL-3.0-only -->
<script>
import { onMount } from 'svelte';
import { api } from './api.js';
import { app, ago, toast } from './store.svelte.js';
import Dialog from './Dialog.svelte';
import Empty from './Empty.svelte';

let rows = $state([]);
let dlg = $state(null);
let f = $state({ version: '', url: '', notes: '' });

async function load() {
  try { rows = await api('GET', '/admin/firmware'); }
  catch (err) { toast(err.message, true); }
}
onMount(load);
$effect(() => { if (app.tab === 'firmware') load(); });

function open() {
  f = { version: '', url: '', notes: '' };
  dlg?.show();
}
async function save(e) {
  e.preventDefault();
  try {
    await api('POST', '/admin/firmware', { version: f.version, url: f.url, notes: f.notes });
    dlg.close(); load(); toast('saved');
  } catch (err) { toast(err.message, true); }
  return false;
}
async function del(id) {
  try { await api('DELETE', '/admin/firmware/' + id); load(); } catch (err) { toast(err.message, true); }
}
async function wipe() {
  if (!confirm('Delete ALL firmware rows?')) return;
  try { await api('DELETE', '/admin/firmware_wipe'); load(); toast('wiped'); }
  catch (err) { toast(err.message, true); }
}
</script>

<div class="card">
  <div class="rowflex">
    <h3>Firmware</h3><span class="sp"></span>
    <button class="act danger" onclick={wipe}>Wipe all</button>
    <button class="pri" onclick={open}>Add firmware</button>
  </div>
  {#if rows.length}
  <table>
    <thead><tr><th>Version</th><th>URL</th><th>Notes</th><th>Added</th><th></th></tr></thead>
    <tbody>
    {#each rows as fw (fw.id)}
      <tr>
        <td><b>{fw.version}</b></td>
        <td class="mono">{fw.url}</td>
        <td>{fw.notes}</td>
        <td>{ago(fw.created_at)}</td>
        <td class="acts"><button class="act danger" onclick={() => del(fw.id)}>Delete</button></td>
      </tr>
    {/each}
    </tbody>
  </table>
  {:else}
    <Empty title="No firmware uploaded"
      blurb="Devices report their own firmware version, and updates only happen when you upload one here. Nothing to do unless you have a newer build."
      action="Add firmware" onaction={open} />
  {/if}
</div>

<Dialog bind:this={dlg}>
  <h3 style="margin-bottom:12px">Add firmware</h3>
  <form onsubmit={save}>
    <div class="row"><label>Version</label><input bind:value={f.version} required></div>
    <div class="row"><label>URL</label><input type="url" bind:value={f.url} required></div>
    <div class="row"><label>Notes</label><input bind:value={f.notes}></div>
    <button class="pri">Save</button>
    <button class="act" type="button" onclick={() => dlg.close()}>Cancel</button>
  </form>
</Dialog>
