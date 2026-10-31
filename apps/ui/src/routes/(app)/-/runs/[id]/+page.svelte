<!--
	/-/runs/<id> — one CI run. App pages live under /-/: `-` is a reserved
	user name, so they never collide with /<owner>/<name>.
-->
<script lang="ts">
	import { page } from '$app/state';
	import Skeleton from '#lib/components/Skeleton.svelte';
	import StateView from '#lib/components/StateView.svelte';
	import { followRun, loadRun, terminal, type RunPage } from '#lib/data/run.ts';
	import { resource } from '#lib/resource.svelte.ts';
	import RunView from '#lib/views/RunView.svelte';

	const id = $derived(page.params.id ?? '');
	const data = resource(() => {
		const runId = id; // read before any await: re-runs when the id changes
		return loadRun(runId);
	});

	// An unfinished run is followed live (ADR 0142); the stream is closed when
	// the page is left, and the page reloads once the run is terminal.
	let live = $state<RunPage>();
	$effect(() => {
		const base = data.data;
		live = undefined;
		if (!base || terminal(base.run.state)) return;
		const stop = new AbortController();
		followRun(base, stop.signal, (p) => (live = p)).then(
			(done) => done && data.retry(),
			() => {} // the log stays as received; reloading the page tries again
		);
		return () => stop.abort();
	});
</script>

<svelte:head><title>Run #{id} · kajiba</title></svelte:head>

{#if data.error}
	<StateView error={data.error} onretry={data.retry} what="run" />
{:else if !data.data}
	<div class="p-6"><Skeleton lines={12} /></div>
{:else}
	<RunView data={live ?? data.data} />
{/if}
