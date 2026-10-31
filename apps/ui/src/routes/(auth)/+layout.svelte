<!--
	Signed-out pages, in the shell's grammar without the forge overview: the
	same thin strip on top, then a Pulse-like band with the one thing to do.
-->
<script lang="ts">
	import ThemeToggle from '#lib/components/ThemeToggle.svelte';

	let { children } = $props();
</script>

<div class="flex min-h-dvh flex-col bg-bg text-fg">
	<header class="shrink-0 border-b border-border bg-surface">
		<div class="flex h-9 items-center gap-2 px-3 text-xs sm:px-4">
			<span class="pl-4 text-[0.8125rem] font-semibold tracking-tight">kajiba</span>
			<span class="text-fg-subtle" lang="ja" aria-hidden="true">鍛冶場</span>
			<ThemeToggle class="ml-auto rounded-sm p-1.5 text-fg-muted hover:bg-surface-2" />
		</div>
	</header>
	<main id="main" class="flex flex-1 flex-col">{@render children()}</main>
</div>
