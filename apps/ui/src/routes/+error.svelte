<!-- Unmatched routes and unexpected errors. -->
<script lang="ts">
	import { page } from '$app/state';
</script>

<svelte:head><title>{page.status} · kajiba</title></svelte:head>

<main class="mx-auto max-w-md px-4 py-24 text-center">
	<p class="font-mono text-3xl font-semibold text-fg-subtle">{page.status}</p>
	<h1 class="mt-2 text-base font-semibold">
		{page.status === 404 ? 'Page not found' : 'Something went wrong'}
	</h1>
	{#if page.status !== 404 && page.error?.message}
		<p class="mt-1 font-mono text-xs text-fg-subtle">{page.error.message}</p>
	{/if}
	<a href="/" class="mt-4 inline-block text-link hover:underline">Back to repositories</a>
</main>
