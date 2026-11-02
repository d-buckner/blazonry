<!-- SPDX-License-Identifier: AGPL-3.0-only -->
<script>
// The one authored empty state (#40). Every tab's "nothing yet" screen is this
// component: the brand dissolve inside a preview frame (the dither law: the
// dither only ever appears inside a screen frame), one plain-English line, and
// one primary action wired to the tab's own create/pair flow.
let { title, blurb, action = null, onaction = null, compact = false } = $props();
// Ordered-dither dissolve, same signature as the app icon: cells fill where
// the diagonal threshold survives, dense at top-left, gone at bottom-right.
const CELLS = '<rect x="24.0" y="24.0" width="22.2" height="19.0"/><rect x="49.2" y="24.0" width="22.2" height="19.0"/><rect x="74.4" y="24.0" width="22.2" height="19.0"/><rect x="99.6" y="24.0" width="22.2" height="19.0"/><rect x="124.8" y="24.0" width="22.2" height="19.0"/><rect x="150.0" y="24.0" width="22.2" height="19.0"/><rect x="175.2" y="24.0" width="22.2" height="19.0"/><rect x="200.4" y="24.0" width="22.2" height="19.0"/><rect x="250.8" y="24.0" width="22.2" height="19.0"/><rect x="24.0" y="46.0" width="22.2" height="19.0"/><rect x="49.2" y="46.0" width="22.2" height="19.0"/><rect x="74.4" y="46.0" width="22.2" height="19.0"/><rect x="99.6" y="46.0" width="22.2" height="19.0"/><rect x="124.8" y="46.0" width="22.2" height="19.0"/><rect x="150.0" y="46.0" width="22.2" height="19.0"/><rect x="175.2" y="46.0" width="22.2" height="19.0"/><rect x="200.4" y="46.0" width="22.2" height="19.0"/><rect x="225.6" y="46.0" width="22.2" height="19.0"/><rect x="24.0" y="68.0" width="22.2" height="19.0"/><rect x="49.2" y="68.0" width="22.2" height="19.0"/><rect x="74.4" y="68.0" width="22.2" height="19.0"/><rect x="99.6" y="68.0" width="22.2" height="19.0"/><rect x="124.8" y="68.0" width="22.2" height="19.0"/><rect x="150.0" y="68.0" width="22.2" height="19.0"/><rect x="24.0" y="90.0" width="22.2" height="19.0"/><rect x="49.2" y="90.0" width="22.2" height="19.0"/><rect x="74.4" y="90.0" width="22.2" height="19.0"/><rect x="99.6" y="90.0" width="22.2" height="19.0"/><rect x="124.8" y="90.0" width="22.2" height="19.0"/><rect x="175.2" y="90.0" width="22.2" height="19.0"/><rect x="49.2" y="112.0" width="22.2" height="19.0"/><rect x="99.6" y="112.0" width="22.2" height="19.0"/><rect x="24.0" y="134.0" width="22.2" height="19.0"/><rect x="74.4" y="134.0" width="22.2" height="19.0"/>';
</script>

<div class="empty{compact ? ' compact' : ''}">
  <div class="empty-frame" aria-hidden="true">
    <svg viewBox="0 0 300 180">{@html CELLS}</svg>
  </div>
  <div class="empty-copy">
    <h3>{title}</h3>
    <p>{blurb}</p>
    {#if action}<button class="pri" onclick={onaction}>{action}</button>{/if}
  </div>
</div>