<!--
	One run, loaded for one id. The page keys it on the id ({#key}), so a link
	to another run starts from scratch: the previous run's request, state and
	live stream (aborted with the component) cannot outlive or overwrite it.
-->
<script lang="ts">
	import Skeleton from '#lib/components/Skeleton.svelte';
	import StateView from '#lib/components/StateView.svelte';
	import { followRun, loadRun, terminal, type RunPage } from '#lib/data/run.ts';
	import { resource } from '#lib/resource.svelte.ts';
	import RunView from '#lib/views/RunView.svelte';

	let { id }: { id: string } = $props();
	const data = resource(() => loadRun(id));

	// An unfinished run is followed live (ADR 0142); the stream is closed when
	// the page is left, and the page reloads once the run is terminal.
	let live = $state<RunPage>();
	$effect(() => {
		const base = data.data;
		live = undefined;
		if (!base || terminal(base.run.state)) return;
		const stop = new AbortController();
		followRun(base, stop.signal, (p) => !stop.signal.aborted && (live = p)).then(
			(done) => done && !stop.signal.aborted && data.retry(),
			() => {} // the log stays as received; reloading the page tries again
		);
		return () => stop.abort();
	});
</script>

{#if data.error}
	<StateView error={data.error} onretry={data.retry} what="run" />
{:else if !data.data}
	<div class="p-6"><Skeleton lines={12} /></div>
{:else}
	<RunView data={live ?? data.data} onchange={data.retry} />
{/if}
