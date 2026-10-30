<!--
	App shell for every signed-in page: sidebar (drawer below `lg`), main area,
	Loads what the sidebar needs: viewer, repositories,
	runners.
-->
<script lang="ts">
	import { afterNavigate } from '$app/navigation';
	import { page } from '$app/state';
	import { Code, getViewer, listRunners, type Runner } from '#lib/api.ts';
	import Icon from '#lib/components/Icon.svelte';
	import Sidebar from '#lib/components/Sidebar.svelte';
	import StateView from '#lib/components/StateView.svelte';
	import Wordmark from '#lib/components/Wordmark.svelte';
	import { allRepos } from '#lib/loaders.ts';
	import { resource } from '#lib/resource.svelte.ts';

	let { children } = $props();

	const viewer = resource(() => getViewer());
	const repos = resource(() => allRepos());
	// Runners are optional chrome: a failure must not break the page.
	const runners = resource<Runner[]>(() =>
		listRunners().then(
			(r) => r.runners,
			() => []
		)
	);

	const current = $derived(
		repos.data?.find((r) => r.owner === page.params.owner && r.name === page.params.name)
	);
	const unauthenticated = $derived(
		(viewer.error as { code?: Code } | undefined)?.code === Code.Unauthenticated
	);

	let drawer = $state(false);
	afterNavigate(() => (drawer = false));
</script>

<div class="min-h-dvh bg-bg text-fg lg:grid lg:grid-cols-[16rem_minmax(0,1fr)]">
	<a href="#main" class="sr-only-focusable fixed top-2 left-2 z-50 bg-surface px-3 py-1"
		>Skip to content</a
	>

	<!-- Small screens: thin bar with the drawer button -->
	<div
		class="sticky top-0 z-30 flex h-12 items-center gap-3 border-b border-border bg-surface px-3 lg:hidden"
	>
		<button
			type="button"
			class="rounded-sm p-1.5 text-fg-muted hover:bg-surface-2"
			aria-expanded={drawer}
			aria-controls="sidebar"
			onclick={() => (drawer = !drawer)}
		>
			<Icon name={drawer ? 'x' : 'menu'} size={18} label={drawer ? 'Close menu' : 'Open menu'} />
		</button>
		<Wordmark />
		{#if current}
			<span class="min-w-0 truncate text-[0.8125rem] text-fg-muted"
				>{current.owner}/<span class="font-medium text-fg">{current.name}</span></span
			>
		{/if}
	</div>

	{#if drawer}
		<button
			type="button"
			class="fixed inset-0 z-30 bg-black/30 lg:hidden"
			aria-label="Close menu"
			onclick={() => (drawer = false)}
		></button>
	{/if}

	<aside
		id="sidebar"
		class="fixed inset-y-0 left-0 z-40 w-[min(18rem,85vw)] border-r border-border bg-surface
			{drawer ? 'block' : 'hidden'} lg:sticky lg:top-0 lg:block lg:h-dvh lg:w-auto"
		aria-label="Navigation"
	>
		<Sidebar repos={repos.data ?? []} {current} viewer={viewer.data} runners={runners.data ?? []} />
	</aside>

	<div id="main" class="min-w-0">
		{#if unauthenticated}
			<StateView error={viewer.error} />
		{:else}
			{@render children()}
		{/if}
	</div>
</div>
