<script>
let el = $state(null);
export function show() { el?.showModal(); }
export function close() { el?.close(); }
</script>

<dialog bind:this={el}><slot /></dialog>
