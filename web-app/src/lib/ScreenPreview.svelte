<!-- SPDX-License-Identifier: AGPL-3.0-only -->
<script>
import { authedBlobUrl } from './api.js';

// The single home of the e-ink preview: a light dot-field (the dither) with the
// device's real rendered screen on top. Previews live behind auth, so <img src>
// can't carry the bearer token — fetch as a blob and point at an object URL.
let { screenId = null, alt = '', ph = '' } = $props();
let src = $state('');

$effect(() => {
  const id = screenId;
  let alive = true, url = '';
  if (id) {
    authedBlobUrl(`/admin/screens/${id}/preview`)
      .then((u) => { if (alive) { src = u; url = u; } else URL.revokeObjectURL(u); })
      .catch(() => {});
  } else {
    src = '';
  }
  return () => { alive = false; if (url) URL.revokeObjectURL(url); };
});
</script>

<div class="ov-screen">
  {#if src}
    <img {src} alt={alt} loading="lazy">
  {:else}
    <span class="ov-ph">{ph}</span>
  {/if}
</div>
