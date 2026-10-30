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
	let { state, label = false }: { state: RunnerState; label?: boolean } = $props();

	const DOT: Record<RunnerState, string> = {
		[RunnerState.UNSPECIFIED]: 'border border-fg-subtle',
		[RunnerState.IDLE]: 'bg-ok',
		[RunnerState.BUSY]: 'bg-accent motion-safe:animate-pulse',
		[RunnerState.OFFLINE]: 'border-[1.5px] border-fg-subtle'
	};
</script>

<span
	class="inline-flex items-center gap-1.5"
	title={label ? undefined : RUNNER_STATE_LABEL[state]}
>
	<span class="inline-block size-2 shrink-0 rounded-full {DOT[state]}" aria-hidden="true"></span>
	{#if label}<span>{RUNNER_STATE_LABEL[state]}</span>{:else}<span class="sr-only"
			>{RUNNER_STATE_LABEL[state]}</span
		>{/if}
</span>
