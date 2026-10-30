<!--
	Uniform loading / error states for every page.
	`error` is an ApiError (or ConnectError — same `.code`) or anything else.
-->
<script lang="ts">
	import { page } from '$app/state';
	import { Code } from '#lib/api.ts';
	import Icon from './Icon.svelte';

	let {
		error,
		onretry,
		what = 'page'
	}: { error: unknown; onretry?: () => void; what?: string } = $props();

	const code = $derived(
		typeof error === 'object' && error && 'code' in error
			? (error as { code: Code }).code
			: undefined
	);
	const message = $derived(error instanceof Error ? error.message : String(error));
</script>

<div class="mx-auto max-w-md px-4 py-16 text-center" role="alert">
	{#if code === Code.NotFound}
		<p class="font-mono text-3xl font-semibold text-fg-subtle">404</p>
		<h1 class="mt-2 text-base font-semibold">Not found</h1>
		<p class="mt-1 text-fg-muted">
			This {what} does not exist, or you don't have access to it.
		</p>
		<a href="/" class="mt-4 inline-block text-link hover:underline">Back to repositories</a>
	{:else if code === Code.Unauthenticated}
		<Icon name="key" size={28} class="mx-auto text-fg-subtle" />
		<h1 class="mt-3 text-base font-semibold">Sign in required</h1>
		<p class="mt-1 text-fg-muted">Your session has expired or you are not signed in.</p>
		<a
			href="/-/login?next={encodeURIComponent(
				page.url.pathname + page.url.search.replace(/[?&]mock=[^&]*/, '')
			)}"
			class="mt-4 inline-flex h-8 items-center rounded-sm bg-accent px-3 font-medium text-accent-fg"
			>Sign in with a passkey</a
		>
	{:else if code === Code.PermissionDenied}
		<Icon name="lock" size={28} class="mx-auto text-fg-subtle" />
		<h1 class="mt-3 text-base font-semibold">No access</h1>
		<p class="mt-1 text-fg-muted">Access is declared in the server configuration.</p>
	{:else}
		<Icon name="x" size={28} class="mx-auto text-danger" />
		<h1 class="mt-3 text-base font-semibold">
			{code === Code.Unavailable ? 'kajiba-server is unreachable' : 'Something went wrong'}
		</h1>
		{#if code !== Code.Unavailable}
			<p class="mt-1 font-mono text-xs break-words text-fg-subtle">{message}</p>
		{:else}
			<p class="mt-1 text-[0.8125rem] text-fg-muted">
				Check that the server is running, then retry.
			</p>
		{/if}
		{#if onretry}
			<button
				type="button"
				onclick={onretry}
				class="mt-4 h-8 rounded-sm border border-border bg-surface px-3 hover:bg-surface-2"
				>Retry</button
			>
		{/if}
	{/if}
</div>
