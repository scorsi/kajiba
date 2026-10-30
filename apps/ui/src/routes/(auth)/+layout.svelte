<!-- Signed-out pages: a single centred card, no navigation. -->
<script lang="ts">
	import ThemeToggle from '#lib/components/ThemeToggle.svelte';
	import Wordmark from '#lib/components/Wordmark.svelte';

	let { children } = $props();
</script>

<div class="flex min-h-dvh flex-col bg-bg text-fg">
	<header class="flex h-14 items-center justify-between px-4">
		<Wordmark />
		<ThemeToggle class="rounded-sm p-1.5 text-fg-muted hover:bg-surface-2 hover:text-fg" />
	</header>
	<main class="flex flex-1 items-start justify-center px-4 pt-[12vh] pb-16">
		<div class="w-full max-w-sm">{@render children()}</div>
	</main>
</div>
