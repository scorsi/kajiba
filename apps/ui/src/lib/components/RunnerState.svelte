<!-- Runner state dot + optional label. Colour is never the only signal: the label or `title` says it. -->
<script lang="ts" module>
	import { RunnerState } from '#lib/api.ts';

	export const RUNNER_STATE_LABEL: Record<RunnerState, string> = {
		[RunnerState.UNSPECIFIED]: 'unknown',
		[RunnerState.IDLE]: 'idle',
		[RunnerState.BUSY]: 'busy',
		[RunnerState.OFFLINE]: 'offline'
	};
</script>

<script lang="ts">
	let {
		state,
		label = false,
		used,
		maxJobs
	}: { state: RunnerState; label?: boolean; used?: number; maxJobs?: number } = $props();
	const description = $derived(
		used === undefined || maxJobs === undefined
			? RUNNER_STATE_LABEL[state]
			: `${used}/${maxJobs} ${
					state === RunnerState.BUSY
						? used >= maxJobs ? 'busy · full' : 'busy · partially busy'
						: RUNNER_STATE_LABEL[state]
				}`
	);

	const DOT: Record<RunnerState, string> = {
		[RunnerState.UNSPECIFIED]: 'border border-fg-subtle',
		[RunnerState.IDLE]: 'bg-ok',
		[RunnerState.BUSY]: 'bg-accent motion-safe:animate-pulse',
		[RunnerState.OFFLINE]: 'border-[1.5px] border-fg-subtle'
	};
</script>

<span
	class="inline-flex items-center gap-1.5"
	title={label ? undefined : description}
>
	<span class="inline-block size-2 shrink-0 rounded-full {DOT[state]}" aria-hidden="true"></span>
	{#if label}<span>{description}</span>{:else}<span class="sr-only"
			>{description}</span
		>{/if}
</span>
