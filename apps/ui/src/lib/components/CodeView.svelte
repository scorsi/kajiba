<!--
	Source view: line numbers as anchors (`#L12`, `#L12-L20`; shift-click extends),
	selected lines highlighted, syntax highlighting loaded lazily. Very large
	inputs skip highlighting.
-->
<script lang="ts">
	import { replaceState } from '$app/navigation';
	import { page } from '$app/state';
	import { highlight } from '#lib/highlight.ts';

	let {
		code,
		language,
		highlightLimit = 256 * 1024
	}: { code: string; language?: string; highlightLimit?: number } = $props();

	const lines = $derived(code.endsWith('\n') ? code.slice(0, -1).split('\n') : code.split('\n'));
	let html = $state<string | undefined>();

	$effect(() => {
		let cancelled = false;
		html = undefined;
		if (language && code.length <= highlightLimit) {
			highlight(code, language).then((h) => !cancelled && (html = h));
		}
		return () => (cancelled = true);
	});

	/** Selected range from the URL hash. */
	const selection = $derived.by(() => {
		const m = /^#L(\d+)(?:-L(\d+))?$/.exec(page.url.hash);
		if (!m) return undefined;
		const a = Number(m[1]);
		const b = Number(m[2] ?? m[1]);
		return { from: Math.min(a, b), to: Math.max(a, b) };
	});

	function onLineClick(e: MouseEvent, n: number) {
		if (e.shiftKey && selection) {
			e.preventDefault();
			const from = Math.min(selection.from, n);
			const to = Math.max(selection.from, n);
			replaceState(`#L${from}-L${to}`, page.state);
		}
	}

	$effect(() => {
		if (selection)
			document.getElementById(`L${selection.from}`)?.scrollIntoView({ block: 'center' });
	});
</script>

<!-- Ligatures off: `->` must read as two characters in source code. -->
<!-- Scrollable region: focusable so keyboard users can scroll it (WCAG 2.1.1). -->
<!-- svelte-ignore a11y_no_noninteractive_tabindex -->
<div
	tabindex="0"
	role="region"
	aria-label="Source code"
	class="relative flex overflow-x-auto font-mono text-[0.8125rem] leading-5 [font-variant-ligatures:none]"
>
	<!-- gutter -->
	<div
		class="tabular sticky left-0 z-[1] shrink-0 border-r border-border bg-surface py-3 text-right text-fg-subtle select-none"
		aria-hidden="true"
	>
		{#each lines as _, i (i)}
			<a
				id="L{i + 1}"
				href="#L{i + 1}"
				tabindex="-1"
				onclick={(e) => onLineClick(e, i + 1)}
				class="block scroll-mt-24 px-3 hover:text-fg {selection &&
				i + 1 >= selection.from &&
				i + 1 <= selection.to
					? 'text-fg'
					: ''}">{i + 1}</a
			>
		{/each}
	</div>
	<!-- code -->
	<div class="relative min-w-0 flex-1 py-3">
		{#if selection}
			<div
				class="pointer-events-none absolute inset-x-0 bg-(--k-line-highlight)"
				style:top="calc(0.75rem + {(selection.from - 1) * 1.25}rem)"
				style:height="{(selection.to - selection.from + 1) * 1.25}rem"
				aria-hidden="true"
			></div>
		{/if}
		<pre class="relative pr-6 pl-4"><code
				>{#if html !== undefined}{@html html}{:else}{code}{/if}</code
			></pre>
	</div>
</div>
