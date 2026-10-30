<!-- kajiba wordmark: plain text, kanji as a quiet companion. -->
<script lang="ts">
	let { href = '/' }: { href?: string } = $props();
</script>

<a {href} class="inline-flex shrink-0 items-baseline gap-1.5" aria-label="kajiba — repositories">
	<span class="text-[0.95rem] font-semibold tracking-tight">kajiba</span>
	<span class="text-xs text-fg-subtle" lang="ja" aria-hidden="true">鍛冶場</span>
</a>
