<!-- SPDX-License-Identifier: AGPL-3.0-only -->
<script>
import { authedBlobUrl } from './api.js';
let { id } = $props();
let src = $state('');
// previews live behind auth, so <img src> can't carry the bearer token —
// fetch as blob and point the img at an object URL
$effect(() => {
  if (!id) return;
  let alive = true;
  authedBlobUrl(`/admin/screens/${id}/preview`)
    .then((u) => alive && (src = u))
    .catch(() => {});
  return () => (alive = false);
});
</script>

{#if src}<img class="prev" {src} loading="lazy" alt="">{/if}
