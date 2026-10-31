<!--
	Uniform error states for every page and band, in the Pulse grammar: a
	status label, one sentence, what to do next.
	`error` is an ApiError (or ConnectError — same `.code`) or anything else.
-->
<script lang="ts">
	import { page } from '$app/state';
	import { Code } from '#lib/api.ts';
	import Icon, { type IconName } from '#lib/components/Icon.svelte';

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
	const next = $derived(
		encodeURIComponent(page.url.pathname + page.url.search)
	);

	const view = $derived.by((): { status: string; icon: IconName; tone: string; title: string } => {
		switch (code) {
			case Code.NotFound:
				return {
					status: '404 · not found',
					icon: 'search',
					tone: 'text-fg-subtle',
					title: `No such ${what}`
				};
			case Code.Unauthenticated:
				return {
					status: '401 · signed out',
					icon: 'key',
					tone: 'text-fg-subtle',
					title: 'Sign in required'
				};
			case Code.PermissionDenied:
				return {
					status: '403 · no access',
					icon: 'lock',
					tone: 'text-fg-subtle',
					title: 'You don’t have access'
				};
			case Code.Unavailable:
				return {
					status: '503 · unavailable',
					icon: 'server',
					tone: 'text-danger',
					title: 'kajiba-server is unreachable'
				};
			default:
				return {
					status: 'error',
					icon: 'alert',
					tone: 'text-danger',
					title: 'Something went wrong'
				};
		}
	});
	const btn =
		'inline-flex h-8 items-center gap-1.5 rounded-sm border border-border bg-bg px-3 text-[0.8125rem] hover:bg-surface-2';
</script>

<section class="border-b border-border bg-surface px-4 py-8 sm:px-6" role="alert">
	<div class="max-w-xl">
		<p class="flex items-center gap-2 text-xs tracking-wide text-fg-subtle uppercase">
			<Icon name={view.icon} size={12} class={view.tone} />{view.status}
		</p>
		<h1 class="mt-1 text-2xl font-semibold tracking-tight">{view.title}</h1>
		<p class="mt-1 text-[0.8125rem] text-fg-muted">
			{#if code === Code.NotFound}
				This {what} does not exist, or it isn’t visible to you.
			{:else if code === Code.Unauthenticated}
				Your session has expired or you are not signed in.
			{:else if code === Code.PermissionDenied}
				Access is declared in the server configuration. Ask an administrator if you need it.
			{:else if code === Code.Unavailable}
				The interface is up but the server doesn’t answer. Check that it is running, then retry.
			{:else}
				An unexpected error. Retrying may help; if not, the server log will say more.
			{/if}
		</p>
		{#if message && ![Code.NotFound, Code.Unauthenticated, Code.PermissionDenied, Code.Unavailable].includes(code as Code)}
			<p class="mt-3 font-mono text-xs break-words text-fg-subtle">{message}</p>
		{/if}
		<div class="mt-5 flex flex-wrap items-center gap-4">
			{#if code === Code.Unauthenticated}
				<a
					href="/-/login?next={next}"
					class="inline-flex h-8 items-center gap-1.5 rounded-sm bg-accent px-3 text-[0.8125rem] font-medium text-accent-fg hover:opacity-90"
					><Icon name="key" size={14} />Sign in with a passkey</a
				>
			{:else if onretry && code !== Code.NotFound && code !== Code.PermissionDenied}
				<button type="button" onclick={onretry} class={btn}>Retry</button>
			{/if}
			<a href="/" class="text-[0.8125rem] text-link hover:underline">Back to activity</a>
		</div>
	</div>
</section>
