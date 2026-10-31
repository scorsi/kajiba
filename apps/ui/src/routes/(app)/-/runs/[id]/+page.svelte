<!--
	/-/runs/<id> — one CI run. App pages live under /-/: `-` is a reserved
	user name, so they never collide with /<owner>/<name>.
-->
<script lang="ts">
	import { page } from '$app/state';
	import RunLoader from '#lib/views/RunLoader.svelte';

	const id = $derived(page.params.id ?? '');
</script>

<svelte:head><title>Run #{id} · kajiba</title></svelte:head>

{#key id}<RunLoader {id} />{/key}
