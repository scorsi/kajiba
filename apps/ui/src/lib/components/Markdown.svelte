<!-- Rendered Markdown: sanitised HTML, then code blocks highlighted lazily. -->
<script lang="ts">
	import { highlightBlocks } from '#lib/highlight.ts';
	import { renderMarkdown } from '#lib/markdown.ts';

	let {
		source,
		linkBase,
		class: klass = ''
	}: { source: string; linkBase?: string; class?: string } = $props();

	let html = $state<string | null>(null);
	let failed = $state(false);
	let container = $state<HTMLElement>();

	$effect(() => {
		if (html !== null && container) highlightBlocks(container);
	});

	$effect(() => {
		let cancelled = false;
		html = null;
		failed = false;
		renderMarkdown(source, { linkBase })
			.then((h) => !cancelled && (html = h))
			.catch(() => !cancelled && (failed = true));
		return () => (cancelled = true);
	});
</script>

{#if failed}
	<pre class="whitespace-pre-wrap {klass}">{source}</pre>
{:else if html === null}
	<div class="space-y-2 {klass}" aria-busy="true" aria-label="Rendering Markdown">
		<div class="h-5 w-1/3 rounded-sm bg-surface-2"></div>
		<div class="h-3 w-full rounded-sm bg-surface-2"></div>
		<div class="h-3 w-5/6 rounded-sm bg-surface-2"></div>
	</div>
{:else}
	<!-- Sanitised by DOMPurify in renderMarkdown(). -->
	<div bind:this={container} class="prose {klass}">{@html html}</div>
{/if}
