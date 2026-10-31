<!--
	The one-line strip a band folds into (and its header when open): ▸/▾,
	label, a summary. A link, so the folded state lives in the URL.
-->
<script lang="ts">
	import type { Snippet } from 'svelte';

	let {
		label,
		open,
		href,
		children
	}: { label: string; open: boolean; href: string; children?: Snippet } = $props();
</script>

<a
	{href}
	data-sveltekit-noscroll
	aria-expanded={open}
	class="flex h-8 shrink-0 items-center gap-3 border-b border-border px-4 text-xs text-fg-muted hover:bg-surface-2 sm:px-6"
>
	<span class="w-3 text-fg-subtle" aria-hidden="true">{open ? '▾' : '▸'}</span>
	<span class="tracking-wide text-fg-subtle uppercase">{label}</span>
	<span class="flex min-w-0 flex-1 items-center gap-3 truncate">{@render children?.()}</span>
</a>
