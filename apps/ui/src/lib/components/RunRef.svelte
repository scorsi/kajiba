<script lang="ts">
	import type { Run } from '#lib/api.ts';
	import { runLabel } from '#lib/run-label.ts';
	let { run }: { run: Run } = $props();
</script>

<span class="min-w-0 break-words text-xs text-fg-muted">
	{#if run.trigger === 'worktree'}<span class="rounded-sm border border-accent/40 bg-accent-soft px-1 text-accent">worktree</span>{' '}{/if}
	<span>{runLabel(run)}</span>
</span>
