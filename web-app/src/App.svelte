<script>
import { onMount } from 'svelte';
import { app, toasts, toast } from './lib/store.svelte.js';
import { api, doLogin, doRegister, logout } from './lib/api.js';
import Devices from './lib/Devices.svelte';
import Screens from './lib/Screens.svelte';
import Playlists from './lib/Playlists.svelte';
import Firmware from './lib/Firmware.svelte';
import Models from './lib/Models.svelte';
import Users from './lib/Users.svelte';

const TABS = ['devices', 'screens', 'playlists', 'firmware', 'models', 'users'];
let authTab = $state('login');
let li = $state({ email: '', pass: '' });
let rg = $state({ email: '', pass: '' });

onMount(async () => {
  const t = sessionStorage.getItem('tr-token');
  if (t) {
    app.token = t;
    try {
      const me = await api('GET', '/admin/me');
      app.email = me.email; app.role = me.role;
    } catch { logout(); }
  }
  app.booted = true;
});

async function login(e) {
  e.preventDefault();
  try { await doLogin(li.email, li.pass); } catch (err) { toast(err.message, true); }
  return false;
}
async function register(e) {
  e.preventDefault();
  try { await doRegister(rg.email, rg.pass); authTab = 'login'; }
  catch (err) { toast(err.message, true); }
  return false;
}
</script>

{#if app.token}
  <header>
    <h1>terminus-rs</h1>
    <nav>
      {#each TABS as t}
        <a class={t === app.tab ? 'on' : ''} href="#{t}" onclick={(e) => { e.preventDefault(); app.tab = t; }}>
          {t[0].toUpperCase() + t.slice(1)}
        </a>
      {/each}
    </nav>
    <span class="sp"></span>
    <span class="mut">{app.email}</span>
    <button class="act" onclick={logout}>Sign out</button>
  </header>
  <main>
    {#if app.tab === 'devices'}<Devices />
    {:else if app.tab === 'screens'}<Screens />
    {:else if app.tab === 'playlists'}<Playlists />
    {:else if app.tab === 'firmware'}<Firmware />
    {:else if app.tab === 'models'}<Models />
    {:else if app.tab === 'users'}<Users />
    {/if}
  </main>
{:else}
  <div id="login" class="card" hidden={authTab !== 'login'}>
    <h2 style="margin-bottom:12px">Sign in</h2>
    <form onsubmit={login}>
      <div class="row"><label>Email</label><input type="email" bind:value={li.email} required></div>
      <div class="row"><label>Password</label><input type="password" bind:value={li.pass} required></div>
      <button class="pri">Sign in</button>
      <p class="mut" style="margin-top:10px">First account registered becomes admin:
        <a href="#" onclick={(e) => { e.preventDefault(); authTab = 'register'; }}>register</a></p>
    </form>
  </div>
  <div id="reg" class="card" hidden={authTab !== 'register'}>
    <h2 style="margin-bottom:12px">Register</h2>
    <form onsubmit={register}>
      <div class="row"><label>Email</label><input type="email" bind:value={rg.email} required></div>
      <div class="row"><label>Password</label><input type="password" bind:value={rg.pass} required minlength="9"></div>
      <button class="pri">Register</button>
      <p class="mut" style="margin-top:10px">An admin must verify new accounts:
        <a href="#" onclick={(e) => { e.preventDefault(); authTab = 'login'; }}>sign in</a></p>
    </form>
  </div>
{/if}

<div id="toasts">
  {#each toasts as t (t.id)}
    <div class="toast{t.err ? ' err' : ''}">{t.msg}</div>
  {/each}
</div>
