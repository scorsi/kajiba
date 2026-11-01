<script lang="ts">
	import { goto } from '$app/navigation';
	import { cancelRun, rerunRun, RunState, type Run } from '#lib/api.ts';
	import { describeError } from '#lib/errors.ts';

	let { run, allowed, rerun = true, onchange = () => {} }: {
		run: Run;
		allowed: boolean;
		rerun?: boolean;
		onchange?: () => void;
	} = $props();
	let confirming = $state(false);
	let busy = $state(false);
	let error = $state('');
	let cancelled = $state(false);
	const active = $derived(!cancelled && (run.state === RunState.QUEUED || run.state === RunState.RUNNING));
	async function act(cancel: boolean) {
		busy = true;
		error = '';
		try {
			if (cancel) {
				await cancelRun(run.id);
				cancelled = true;
				confirming = false;
				onchange();
			} else {
				await goto(`/-/runs/${await rerunRun(run.id)}`);
			}
		} catch (e) {
			error = describeError(e).message;
		} finally {
			busy = false;
		}
	}
</script>

{#if allowed}
	<div class="flex flex-wrap items-center gap-3 text-xs" aria-label="Run actions">
		{#if active}
			{#if confirming}
				<span>Cancel this run?</span>
				<button class="cursor-pointer text-danger hover:underline disabled:opacity-50" disabled={busy} onclick={() => act(true)}>Confirm cancel</button>
				<button class="cursor-pointer text-fg-muted hover:underline" disabled={busy} onclick={() => confirming = false}>Keep running</button>
			{:else}
				<button class="cursor-pointer text-danger hover:underline" disabled={busy} onclick={() => confirming = true}>Cancel</button>
			{/if}
		{/if}
		{#if rerun}
			<button class="cursor-pointer text-accent hover:underline disabled:opacity-50" disabled={busy} onclick={() => act(false)}>Re-run</button>
		{/if}
		{#if error}<p class="text-danger" role="alert">{error}</p>{/if}
	</div>
{/if}
