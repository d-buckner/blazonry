<script>
import { onMount } from 'svelte';
import { api } from './api.js';
import { app, toast } from './store.svelte.js';

let rows = $state([]);

async function load() {
  try { rows = await api('GET', '/admin/users'); }
  catch (err) { toast(err.message, true); }
}
onMount(load);
$effect(() => { if (app.tab === 'users') load(); });

async function verify(id) {
  try { await api('POST', '/admin/users/verify', { user_id: id }); load(); toast('verified'); }
  catch (err) { toast(err.message, true); }
}
</script>

<div class="card">
  <h3 style="margin-bottom:10px">Users</h3>
  <table>
    <thead><tr><th>Email</th><th>Role</th><th></th></tr></thead>
    <tbody>
    {#each rows as u (u.id)}
      <tr>
        <td>{u.email}</td>
        <td><span class="pill {u.role === 'unverified' ? 'warn' : 'ok'}">{u.role}</span></td>
        <td class="acts">
          {#if u.role === 'unverified'}
            <button class="act" onclick={() => verify(u.id)}>Verify</button>
          {/if}
        </td>
      </tr>
    {/each}
    </tbody>
  </table>
</div>
