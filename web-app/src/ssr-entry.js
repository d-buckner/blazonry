// Server-side prerender entry: renders the logged-out shell to an HTML string.
import { render } from 'svelte/server';
import App from './App.svelte';

export function renderApp() {
  return render(App);
}
