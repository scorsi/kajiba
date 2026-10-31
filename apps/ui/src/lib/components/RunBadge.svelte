<!-- CI run / step state: icon + optional label. Never colour alone (title + sr text). -->
<script lang="ts" module>
	import { RunState } from '#lib/api.ts';

	export const RUN_LABEL: Record<RunState, string> = {
		[RunState.UNSPECIFIED]: 'unknown',
		[RunState.QUEUED]: 'queued',
		[RunState.RUNNING]: 'running',
		[RunState.SUCCEEDED]: 'passed',
		[RunState.FAILED]: 'failed',
		[RunState.CANCELLED]: 'cancelled'
	};
</script>

<script lang="ts">
	let {
		state,
		label = false,
		size = 12
	}: { state: RunState; label?: boolean; size?: number } = $props();

	const COLOR: Record<RunState, string> = {
		[RunState.UNSPECIFIED]: 'text-fg-subtle',
		[RunState.QUEUED]: 'text-fg-subtle',
		[RunState.RUNNING]: 'text-accent',
		[RunState.SUCCEEDED]: 'text-ok',
		[RunState.FAILED]: 'text-danger',
		[RunState.CANCELLED]: 'text-fg-subtle'
	};
</script>

<span
	class="inline-flex items-center gap-1 {COLOR[state]}"
	title={label ? undefined : RUN_LABEL[state]}
>
	<svg width={size} height={size} viewBox="0 0 12 12" aria-hidden="true" class="shrink-0">
		{#if state === RunState.SUCCEEDED}
			<circle cx="6" cy="6" r="5.25" fill="currentColor" />
			<path
				d="M3.5 6.2l1.7 1.6 3.3-3.4"
				fill="none"
				stroke="var(--k-surface)"
				stroke-width="1.5"
				stroke-linecap="round"
				stroke-linejoin="round"
			/>
		{:else if state === RunState.FAILED}
			<circle cx="6" cy="6" r="5.25" fill="currentColor" />
			<path
				d="M4.2 4.2l3.6 3.6M7.8 4.2l-3.6 3.6"
				stroke="var(--k-surface)"
				stroke-width="1.5"
				stroke-linecap="round"
			/>
		{:else if state === RunState.RUNNING}
			<circle
				cx="6"
				cy="6"
				r="4.5"
				fill="none"
				stroke="currentColor"
				stroke-width="1.5"
				stroke-dasharray="20 9"
				class="origin-center motion-safe:animate-spin"
			/>
		{:else if state === RunState.CANCELLED}
			<circle cx="6" cy="6" r="4.75" fill="none" stroke="currentColor" stroke-width="1.5" />
			<path d="M3.2 8.8l5.6-5.6" stroke="currentColor" stroke-width="1.5" />
		{:else}
			<circle
				cx="6"
				cy="6"
				r="4.75"
				fill="none"
				stroke="currentColor"
				stroke-width="1.5"
				stroke-dasharray="2 2"
			/>
		{/if}
	</svg>
	{#if label}<span>{RUN_LABEL[state]}</span>{:else}<span class="sr-only">{RUN_LABEL[state]}</span
		>{/if}
</span>
