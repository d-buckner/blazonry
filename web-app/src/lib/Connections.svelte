<!-- SPDX-License-Identifier: AGPL-3.0-only -->
<script>
import { onMount } from 'svelte';
import { api } from './api.js';
import { app, toast } from './store.svelte.js';
import Dialog from './Dialog.svelte';
import Empty from './Empty.svelte';

let providers = $state([]);
let extensions = $state([]);
// The plugin catalog: every extension and provider kind this build ships,
// described by the manifest in its folder. Both forms below render from it,
// so a new extension needs no change here.
let kinds = $state({ extensions: [], providers: [] });
let pdlg = $state(null);
let xdlg = $state(null);

let pf = $state({ kind: '', name: '', refresh: '' });
let pvals = $state({});
let xf = $state({ kind: '', name: '' });
let xvals = $state({});

// A provider kind may offer a live lookup while the form is still being filled
// — Home Assistant lists its real tab names, which no REST endpoint exposes.
// The answer names the fields it fills and which one drives a re-run, so this
// component never needs to know which kind can do it.
let probe = $state({ fields: {}, driver: '', note: '', busy: false });

const pkind = $derived(kinds.providers.find(p => p.kind === pf.kind) || {});
const xkind = $derived(kinds.extensions.find(e => e.kind === xf.kind) || {});
const plabel = k => kinds.providers.find(p => p.kind === k)?.label ?? k;
const xlabel = k => kinds.extensions.find(e => e.kind === k)?.label ?? k;

async function load() {
  try {
    [providers, extensions, kinds] = await Promise.all([
      api('GET', '/admin/providers'),
      api('GET', '/admin/extensions'),
      api('GET', '/admin/kinds'),
    ]);
  } catch (err) { toast(err.message, true); }
}
onMount(load);
$effect(() => { if (app.tab === 'connections') load(); });

function providersFor(shape) {
  return providers.filter(p => p.shape === shape);
}

// A field starts at its declared default; a field with none starts empty,
// which the server reads as "derive it" or "all of them".
function defaultsFor(fields) {
  const o = {};
  for (const f of (fields || [])) {
    o[f.key] = f.default !== undefined ? f.default
      : f.type === 'bool' ? false : f.type === 'provider_ids' ? [] : '';
  }
  return o;
}

// ---------- providers ----------
function openP() {
  const k = kinds.providers[0] || {};
  pf = { kind: k.kind || '', name: '', refresh: '' };
  pvals = defaultsFor(k.config);
  probe = { fields: {}, driver: '', note: '', busy: false };
  pdlg?.show();
}

// Ask the kind to fill its own fields. An unreachable instance answers with a
// reason instead of a failed request, so the dialog shows it inline.
async function probeFields() {
  probe.busy = true; probe.note = '';
  try {
    const r = await api('POST', `/admin/providers/${pf.kind}/browse`, buildConfig());
    if (r.ok === false) {
      probe.fields = {};
      probe.note = r.error;
    } else {
      probe.fields = r.fields || {};
      probe.driver = r.driver || '';
      probe.note = r.note || '';
      // Fill a field the operator left blank with the first choice offered.
      for (const [key, opts] of Object.entries(probe.fields)) {
        if (opts.length && !pvals[key]) pvals[key] = opts[0].value;
      }
    }
  } catch (err) { probe.note = err.message; }
  probe.busy = false;
}

function buildConfig() {
  const c = {};
  for (const f of pkind.config || []) {
    const v = pvals[f.key];
    if (v === '' || v === null || v === undefined) continue;
    c[f.key] = f.type === 'number' ? +v : v;
  }
  return c;
}

async function saveP(e) {
  e.preventDefault();
  const body = { kind: pf.kind, name: pf.name || pkind.label, config: buildConfig() };
  // A wall display goes stale fast; each kind declares its own starting cadence.
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
  const k = kinds.extensions[0] || {};
  xf = { kind: k.kind || '', name: '' };
  xvals = defaultsFor(k.settings);
  xdlg?.show();
}

// Blank optional text stays out of the payload: absent means "derive it from
// the data", empty string would mean a blank title.
function buildSettings() {
  const s = {};
  for (const f of xkind.settings || []) {
    const v = xvals[f.key];
    if (f.type === 'provider_ids') { if (v && v.length) s[f.key] = v.map(Number); continue; }
    if (f.type === 'provider_id' || f.type === 'int' || f.type === 'number') {
      if (v !== '' && v !== null && v !== undefined) s[f.key] = +v;
      continue;
    }
    if (f.type === 'bool') { s[f.key] = !!v; continue; }
    if (v === '' || v === null || v === undefined) continue;
    s[f.key] = v;
  }
  return s;
}

async function saveX(e) {
  e.preventDefault();
  const body = { kind: xf.kind, name: xf.name || xkind.label, settings: buildSettings() };
  try {
    const out = await api('POST', '/admin/extensions', body);
    xdlg.close(); load();
    toast(`Created — screen #${out.screen_id} added; add it to a playlist to show it`);
  } catch (err) { toast(err.message, true); }
  return false;
}
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
    <h3>Sources</h3><span class="sp"></span>
    <button class="pri" onclick={openP}>Add source</button>
  </div>
  {#if providers.length === 0}
    <Empty title="No sources connected"
      blurb="A source pulls live information — your calendar, the weather, or a Home Assistant tab — and feeds the extension screens below."
      action="Add source" onaction={openP} />
  {:else}
    <table>
      <thead><tr><th>Name</th><th>Kind</th><th>Last fetch</th><th>Status</th><th></th></tr></thead>
      <tbody>
      {#each providers as p (p.id)}
        <tr>
          <td>{p.name} <span class="mut">#{p.id}</span></td>
          <td>{plabel(p.kind)}</td>
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
    <p class="mut">Extensions turn your data into ready-made screens — a calendar agenda, a weather card, or a Home Assistant tab. Add one, then drop its screen into a playlist.</p>
  {:else}
    <table>
      <thead><tr><th>Name</th><th>Kind</th><th>Screen</th><th></th></tr></thead>
      <tbody>
      {#each extensions as x (x.id)}
        <tr>
          <td>{x.name} <span class="mut">#{x.id}</span></td>
          <td>{xlabel(x.kind)}</td>
          <td class="mut">#{x.screen_id}</td>
          <td class="acts"><button class="act danger" onclick={() => delX(x)}>Delete</button></td>
        </tr>
      {/each}
      </tbody>
    </table>
  {/if}
</div>

<Dialog bind:this={pdlg}>
  <h3 style="margin-bottom:12px">Add source</h3>
  <form onsubmit={saveP}>
    <div class="row"><label>Kind</label>
      <select bind:value={pf.kind} class="ctl"
        onchange={() => { pvals = defaultsFor(pkind.config); }}>
        {#each kinds.providers as k (k.kind)}<option value={k.kind}>{k.label}</option>{/each}
      </select>
    </div>
    {#if pkind.blurb}<p class="mut blurb">{pkind.blurb}</p>{/if}
    <div class="row"><label>Name</label><input bind:value={pf.name} placeholder={pkind.label}></div>
    {#each pkind.config || [] as f (f.key)}
      {@const choices = probe.fields[f.key] || []}
      <div class="row"><label>{f.label}</label>
        {#if choices.length}
          <select bind:value={pvals[f.key]} class="ctl"
            onchange={() => { if (f.key === probe.driver) probeFields(); }}>
            {#each choices as c (c.value)}<option value={c.value}>{c.label}</option>{/each}
          </select>
        {:else if f.type === 'enum'}
          <select bind:value={pvals[f.key]} class="ctl">
            {#each f.values as v (v)}<option value={v}>{v}</option>{/each}
          </select>
        {:else if f.type === 'number'}
          <input type="number" step="any" bind:value={pvals[f.key]} required={!f.optional && f.default === undefined}>
        {:else}
          <input type={f.secret ? 'password' : 'text'} bind:value={pvals[f.key]} autocomplete="off"
            required={!f.optional && f.default === undefined}>
        {/if}
      </div>
      {#if f.hint}<p class="mut blurb">{f.hint}</p>{/if}
    {/each}
    {#if pkind.browse}
      <div class="row"><label></label>
        <button class="act" type="button" onclick={probeFields} disabled={probe.busy}
          >{probe.busy ? 'Looking…' : 'Find options'}</button>
      </div>
      {#if probe.note}<p class="mut blurb">{probe.note}</p>{/if}
    {/if}
    <div class="row"><label>Refresh (secs)</label><input type="number" min="60" bind:value={pf.refresh}
      placeholder={pkind.default_refresh_secs ?? 3600}></div>
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
      <select bind:value={xf.kind} class="ctl"
        onchange={() => { xvals = defaultsFor(xkind.settings); }}>
        {#each kinds.extensions as k (k.kind)}<option value={k.kind}>{k.label}</option>{/each}
      </select>
    </div>
    {#if xkind.blurb}<p class="mut blurb">{xkind.blurb}</p>{/if}
    <div class="row"><label>Name</label><input bind:value={xf.name} placeholder={xkind.label}></div>
    {#each xkind.settings || [] as f (f.key)}
      {@const cands = f.type === 'provider_id' || f.type === 'provider_ids' ? providersFor(xkind.consumes) : []}
      <div class="row"><label>{f.label}</label>
        {#if f.type === 'provider_ids'}
          {#if cands.length === 0}
            <p class="mut" style="margin:0">No {xkind.consumes} sources yet — add one above first.</p>
          {:else}
            <select multiple size="3" bind:value={xvals[f.key]} class="ctl">
              {#each cands as p (p.id)}<option value={p.id}>{p.name}</option>{/each}
            </select>
          {/if}
        {:else if f.type === 'provider_id'}
          {#if cands.length === 0}
            <p class="mut" style="margin:0">No {xkind.consumes} sources yet — add one above first.</p>
          {:else}
            <select bind:value={xvals[f.key]} class="ctl">
              <option value="">Default</option>
              {#each cands as p (p.id)}<option value={p.id}>{p.name}</option>{/each}
            </select>
          {/if}
        {:else if f.type === 'enum'}
          <select bind:value={xvals[f.key]} class="ctl">
            {#each f.values as v (v)}<option value={v}>{v}</option>{/each}
          </select>
        {:else if f.type === 'bool'}
          <label><input type="checkbox" bind:checked={xvals[f.key]}> {f.hint || f.label}</label>
        {:else if f.type === 'int' || f.type === 'number'}
          <input type="number" step={f.type === 'number' ? 'any' : '1'}
            min={f.min} max={f.max} bind:value={xvals[f.key]}>
        {:else if f.type === 'date'}
          <input type="date" bind:value={xvals[f.key]}>
        {:else}
          <input bind:value={xvals[f.key]}>
        {/if}
      </div>
      {#if f.hint && f.type !== 'bool'}<p class="mut blurb">{f.hint}</p>{/if}
      {#if f.type === 'provider_ids' && cands.length}<p class="mut blurb">Nothing selected = all of them.</p>{/if}
    {/each}
    <div class="dlg-actions">
      <button class="pri">Create</button>
      <button class="act" type="button" onclick={() => xdlg.close()}>Cancel</button>
    </div>
  </form>
</Dialog>
