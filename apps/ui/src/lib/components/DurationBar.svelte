<script lang="ts">
	import { dur } from '#lib/data/forge.ts';

	let { seconds, max }: { seconds: number; max: number } = $props();
</script>

<span class="inline-grid shrink-0 grid-cols-[6rem_8ch] items-center gap-2 text-xs">
	<span class="h-1 rounded-full bg-border" aria-hidden="true">
		<span
			class="block h-full rounded-full bg-accent"
			style:width="{Math.min(100, Math.max(0, seconds) / Math.max(1, max) * 100)}%"
		></span>
	</span>
	<span class="tabular text-right text-fg-subtle">{dur(seconds)}</span>
</span>
