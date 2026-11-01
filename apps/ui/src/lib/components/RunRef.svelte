<script lang="ts">
	import type { Run } from '#lib/api.ts';
	import { runLabel } from '#lib/run-label.ts';
	let { run }: { run: Run } = $props();
</script>

<span class="min-w-0 break-words text-xs text-fg-muted">
	{#if run.trigger === 'worktree' || run.ref}<span class="rounded-sm border border-accent/40 bg-accent-soft px-1 text-accent">{run.trigger === 'worktree' ? 'worktree' : runLabel(run)}</span>{/if}
	{#if run.trigger === 'worktree'}{' '}<span>{runLabel(run)}</span>{/if}
</span>
