<!--
	Disclosure popover on <details>/<summary>: keyboard (Enter/Space) and screen
	readers for free. Adds Escape and click-outside to close.
-->
<script lang="ts">
	import type { Snippet } from 'svelte';

	let {
		trigger,
		children,
		open = $bindable(false),
		align = 'right',
		width = '22rem',
		triggerClass = '',
		label,
		class: klass = ''
	}: {
		trigger: Snippet;
		children: Snippet;
		open?: boolean;
		align?: 'left' | 'right';
		width?: string;
		triggerClass?: string;
		label?: string;
		class?: string;
	} = $props();

	let el: HTMLDetailsElement;

	function onWindowClick(e: MouseEvent) {
		if (open && el && !el.contains(e.target as Node)) open = false;
	}
	function onKey(e: KeyboardEvent) {
		if (open && e.key === 'Escape') {
			open = false;
			el.querySelector('summary')?.focus();
		}
	}
</script>

<svelte:window onclick={onWindowClick} onkeydown={onKey} />

<details bind:this={el} bind:open class="relative {klass}">
	<summary
		class="flex cursor-pointer list-none items-center [&::-webkit-details-marker]:hidden {triggerClass}"
		aria-label={label}
	>
		{@render trigger()}
	</summary>
	<div
		class="absolute z-30 mt-1.5 max-w-[calc(100vw-1rem)] overflow-hidden rounded-md border border-border bg-surface text-fg shadow-lg {align ===
		'right'
			? 'right-0'
			: 'left-0'}"
		style:width
	>
		{@render children()}
	</div>
</details>
