<!-- SPDX-License-Identifier: AGPL-3.0-only -->
<script>
let el = $state(null);
export function show() { el?.showModal(); }
export function close() { el?.close(); }
</script>

<dialog bind:this={el}><slot /></dialog>
