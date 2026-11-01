/* SPDX-License-Identifier: AGPL-3.0-only */
import { mount, hydrate } from 'svelte';
import App from './App.svelte';

// If the server rendered markup is present, hydrate it; otherwise mount fresh.
const target = document.getElementById('app');
if (target && target.innerHTML.trim()) {
  hydrate(App, { target });
} else {
  mount(App, { target: document.body });
}
