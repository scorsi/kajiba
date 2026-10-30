<!-- Placeholder lines while data loads. Announced once via aria-busy on the container. -->
<script lang="ts">
	let { lines = 6, class: klass = '' }: { lines?: number; class?: string } = $props();
	const widths = ['w-2/5', 'w-4/5', 'w-3/5', 'w-11/12', 'w-1/2', 'w-3/4', 'w-2/3', 'w-5/6'];
</script>

<div class="space-y-2.5 {klass}" aria-busy="true" aria-label="Loading">
	{#each { length: lines }, i (i)}
		<div
			class="h-3.5 rounded-sm bg-surface-2 motion-safe:animate-pulse {widths[i % widths.length]}"
		></div>
	{/each}
</div>
