<!--
	/-/runs/<id> — one CI run. App pages live under /-/: `-` is a reserved
	user name, so they never collide with /<owner>/<name>.
-->
<script lang="ts">
	import { page } from '$app/state';
	import Skeleton from '#lib/components/Skeleton.svelte';
	import StateView from '#lib/components/StateView.svelte';
	import { loadRun } from '#lib/data/run.ts';
	import { resource } from '#lib/resource.svelte.ts';
	import RunView from '#lib/views/RunView.svelte';

	const id = $derived(page.params.id ?? '');
	const data = resource(() => {
		const runId = id; // read before any await: re-runs when the id changes
		return loadRun(runId);
	});
</script>

<svelte:head><title>Run #{id} · kajiba</title></svelte:head>

{#if data.error}
	<StateView error={data.error} onretry={data.retry} what="run" />
{:else if !data.data}
	<div class="p-6"><Skeleton lines={12} /></div>
{:else}
	<RunView data={data.data} />
{/if}
