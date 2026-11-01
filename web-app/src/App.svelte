<script>
import { onMount } from 'svelte';
import { app, toasts, toast } from './lib/store.svelte.js';
import { api, doLogin, doRegister, logout } from './lib/api.js';
import Devices from './lib/Devices.svelte';
import Screens from './lib/Screens.svelte';
import Playlists from './lib/Playlists.svelte';
import Connections from './lib/Connections.svelte';
import Firmware from './lib/Firmware.svelte';
import Models from './lib/Models.svelte';
import Users from './lib/Users.svelte';

const TABS = ['devices', 'screens', 'playlists', 'connections', 'firmware', 'models', 'users'];
let authTab = $state('login');
let authMode = $state('local');
let li = $state({ email: '', pass: '' });
let rg = $state({ email: '', pass: '' });

onMount(async () => {
  // OIDC callback lands here as /#mntr_token=... — adopt and scrub the URL.
  const m = location.hash.match(/mntr_token=([0-9a-f]+)/);
  if (m) {
    sessionStorage.setItem('tr-token', m[1]);
    history.replaceState(null, '', '/');
  }
  try { authMode = (await api('GET', '/admin/auth/mode')).mode; } catch {}
  const t = sessionStorage.getItem('tr-token');
  if (t) {
    app.token = t;
    try {
      const me = await api('GET', '/admin/me');
      app.email = me.email; app.role = me.role;
    } catch { logout(); }
  } else if (authMode === 'none') {
    // open server: enter directly, no session needed
    app.token = 'open'; app.email = 'open'; app.role = 'admin';
  }
  app.booted = true;
});

// PWA signals from the shell registration (see build.js inline script).
function onNet(e) { app.online = e.detail === 'online'; }
function onUpdate() { app.updateReady = true; }
function applyUpdate() {
  navigator.serviceWorker.getRegistration().then((r) => r && r.waiting && r.waiting.postMessage('skip-waiting'));
}
if (typeof window !== 'undefined') {
  window.addEventListener('mntr-net', onNet);
  window.addEventListener('mntr-update', onUpdate);
}

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

{#if !app.online}
  <div id="offline-banner">Offline — mntr server unreachable. Data shown may be stale.</div>
{/if}
{#if app.updateReady}
  <div id="update-banner">An update is available. <button class="act" onclick={applyUpdate}>Reload</button></div>
{/if}
{#if app.token}
  <header>
    <div class="brand">
      <h1>mntr</h1>
    </div>
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
    {:else if app.tab === 'connections'}<Connections />
    {:else if app.tab === 'firmware'}<Firmware />
    {:else if app.tab === 'models'}<Models />
    {:else if app.tab === 'users'}<Users />
    {/if}
  </main>
{:else}
  {#if authMode === 'oidc'}
  <div id="login" class="card">
    <h2 style="margin-bottom:12px">Sign in</h2>
    <p class="mut" style="margin-bottom:12px">This server uses single sign-on.</p>
    <a class="pri" href="/auth/oidc/login" style="display:inline-block;text-decoration:none">Sign in with SSO</a>
  </div>
  {:else}
  <div id="login" class="card" hidden={authTab !== 'login'}>
    <h2 style="margin-bottom:12px">Sign in</h2>
    <form onsubmit={login}>
      {#if authMode === 'ldap'}
      <div class="row"><label>Username</label><input type="text" bind:value={li.email} required></div>
      {:else}
      <div class="row"><label>Email</label><input type="email" bind:value={li.email} required></div>
      {/if}
      <div class="row"><label>Password</label><input type="password" bind:value={li.pass} required></div>
      <button class="pri">Sign in</button>
      {#if authMode === 'local'}
      <p class="mut" style="margin-top:10px">First account registered becomes admin:
        <a href="#" onclick={(e) => { e.preventDefault(); authTab = 'register'; }}>register</a></p>
      {/if}
    </form>
  </div>
  {/if}
  {#if authMode === 'local'}
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
{/if}

<div id="toasts">
  {#each toasts as t (t.id)}
    <div class="toast{t.err ? ' err' : ''}">{t.msg}</div>
  {/each}
</div>
