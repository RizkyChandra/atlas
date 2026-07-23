<script lang="ts">
  import { onMount } from 'svelte'
  import { store } from './store'

  export let title = 'hi'

  function handle() {
    store.set(title)
  }
</script>

<button on:click={handle}>{title}</button>

{#await import('./Modal.svelte') then Modal}
  <svelte:component this={Modal} />
{/await}
