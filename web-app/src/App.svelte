<!-- SPDX-License-Identifier: AGPL-3.0-only -->
<script>
import { onMount } from 'svelte';
import { app, toasts, toast } from './lib/store.svelte.js';
import { api, doLogin, doRegister, logout } from './lib/api.js';
import Devices from './lib/Devices.svelte';
import Welcome from './lib/Welcome.svelte';
import Screens from './lib/Screens.svelte';
import Playlists from './lib/Playlists.svelte';
import Connections from './lib/Connections.svelte';
import Firmware from './lib/Firmware.svelte';
import Models from './lib/Models.svelte';
import Users from './lib/Users.svelte';

const TABS = ['devices', 'screens', 'playlists', 'connections', 'firmware', 'models', 'users'];
let authTab = $state('login');
let authMode = $state('local');
let firstRun = $state(false);
let li = $state({ email: '', pass: '' });
let rg = $state({ email: '', pass: '' });

onMount(async () => {
  // OIDC callback lands here as /#blazonry_token=... — adopt and scrub the URL.
  const m = location.hash.match(/blazonry_token=([0-9a-f]+)/);
  if (m) {
    sessionStorage.setItem('bz-token', m[1]);
    history.replaceState(null, '', '/');
  }
  try {
    const am = await api('GET', '/admin/auth/mode');
    authMode = am.mode;
    firstRun = !!am.first_run;
    if (firstRun) authTab = 'register';
  } catch {}
  const t = sessionStorage.getItem('bz-token');
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
  window.addEventListener('blazonry-net', onNet);
  window.addEventListener('blazonry-update', onUpdate);
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
  <div id="offline-banner">Offline — blazonry server unreachable. Data shown may be stale.</div>
{/if}
{#if app.updateReady}
  <div id="update-banner">An update is available. <button class="act" onclick={applyUpdate}>Reload</button></div>
{/if}
{#if app.token}
  <header>
    <div class="brand">
      <h1>blazonry</h1>
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
    {#if app.tab === 'devices' && app.role === 'admin'}<Welcome />{/if}
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
<div id="auth">
  <div class="auth-brand">
    <h1>blazonry</h1>
    <p class="tag">your displays, your server</p>
  </div>
  {#if authMode === 'oidc'}
  <div id="login" class="card auth-card">
    <h2>Sign in</h2>
    <p class="mut" style="margin-bottom:16px">This server uses single sign-on.</p>
    <a class="pri auth-cta" href="/auth/oidc/login">Sign in with SSO</a>
  </div>
  {:else if authTab === 'register'}
  <div id="reg" class="card auth-card">
    <h2>{firstRun ? 'Create your account' : 'Create an account'}</h2>
    {#if firstRun}<p class="mut auth-note">You're the first account on this server — it becomes the admin that manages everything.</p>{/if}
    <form onsubmit={register}>
      <label class="field"><span>Email</span><input type="email" bind:value={rg.email} required autocomplete="username"></label>
      <label class="field"><span>Password</span><input type="password" bind:value={rg.pass} required minlength="9" autocomplete="new-password"></label>
      <button class="pri auth-cta">{firstRun ? 'Create admin account' : 'Create account'}</button>
    </form>
    <p class="auth-alt">Already have an account?
      <a href="#" onclick={(e) => { e.preventDefault(); authTab = 'login'; }}>Sign in</a></p>
  </div>
  {:else}
  <div id="login" class="card auth-card">
    <h2>Sign in</h2>
    <form onsubmit={login}>
      {#if authMode === 'ldap'}
      <label class="field"><span>Username</span><input type="text" bind:value={li.email} required autocomplete="username"></label>
      {:else}
      <label class="field"><span>Email</span><input type="email" bind:value={li.email} required autocomplete="username"></label>
      {/if}
      <label class="field"><span>Password</span><input type="password" bind:value={li.pass} required autocomplete="current-password"></label>
      <button class="pri auth-cta">Sign in</button>
    </form>
    {#if authMode === 'local'}
    <p class="auth-alt">New here?
      <a href="#" onclick={(e) => { e.preventDefault(); authTab = 'register'; }}>Create an account</a></p>
    {/if}
  </div>
  {/if}
</div>
{/if}

<div id="toasts">
  {#each toasts as t (t.id)}
    <div class="toast{t.err ? ' err' : ''}">{t.msg}</div>
  {/each}
</div>
