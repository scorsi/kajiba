<!--
	Repository frame: resolves the repository (following renames), its refs,
	and renders the header shared by Code / History / commit pages.
-->
<script lang="ts">
	import { goto } from '$app/navigation';
	import { page } from '$app/state';
	import { listRefs } from '#lib/api.ts';
	import CloneButton from '#lib/components/CloneButton.svelte';
	import Icon from '#lib/components/Icon.svelte';
	import Skeleton from '#lib/components/Skeleton.svelte';
	import StateView from '#lib/components/StateView.svelte';
	import { setRepoContext } from '#lib/context.ts';
	import { findRepo } from '#lib/loaders.ts';
	import { resource } from '#lib/resource.svelte.ts';

	let { children } = $props();

	const res = resource(async () => {
		const { repo, renamedFrom } = await findRepo(page.params.owner!, page.params.name!);
		const key = `${repo.owner}/${repo.name}`;
		const { refs } = await listRefs({ repo: key });
		return { repo, renamedFrom, refs, key };
	});

	// Old name in the URL: replace it, keep the rest of the path.
	$effect(() => {
		const d = res.data;
		if (d?.renamedFrom && page.params.name === d.renamedFrom) {
			const url = new URL(page.url.href);
			url.pathname =
				`/${d.repo.owner}/${d.repo.name}` +
				url.pathname.slice(`/${d.repo.owner}/${d.renamedFrom}`.length);
			url.searchParams.set('renamed', d.renamedFrom);
			goto(url, { replaceState: true });
		}
	});

	setRepoContext({
		get repo() {
			return res.data!.repo;
		},
		get refs() {
			return res.data!.refs;
		},
		get key() {
			return res.data!.key;
		}
	});

	const renamedFrom = $derived(page.url.searchParams.get('renamed'));
</script>

<svelte:head>
	<title>{page.params.owner}/{page.params.name} · kajiba</title>
</svelte:head>

<main class="px-4 pt-5 pb-16 sm:px-6 lg:px-8 lg:pt-7">
	{#if res.error && !res.data}
		<StateView error={res.error} what="repository" onretry={res.retry} />
	{:else if !res.data}
		<Skeleton lines={3} class="max-w-md" />
	{:else}
		{@const repo = res.data.repo}
		<header class="flex flex-wrap items-start gap-x-6 gap-y-3">
			<div class="min-w-0 flex-1">
				<h1 class="flex flex-wrap items-center gap-x-1.5 text-xl tracking-tight">
					<span class="text-fg-muted">{repo.owner}</span><span class="text-fg-subtle">/</span><span
						class="font-semibold">{repo.name}</span
					>
					{#if repo.archived}
						<span
							class="ml-1 inline-flex items-center gap-1 rounded-full bg-warn-soft px-2 py-0.5 text-xs text-warn"
						>
							<Icon name="archive" size={12} /> Archived
						</span>
					{/if}
				</h1>
				{#if repo.description}<p class="mt-1 text-fg-muted">{repo.description}</p>{/if}
			</div>
			{#if !repo.archived && res.data.refs.length}
				<CloneButton owner={repo.owner} name={repo.name} />
			{/if}
		</header>

		{#if repo.archived}
			<p
				class="mt-4 flex items-center gap-2 rounded-md bg-warn-soft px-3 py-2 text-[0.8125rem] text-warn"
			>
				<Icon name="archive" size={14} /> Archived: browsable, but git clone and push are refused.
			</p>
		{/if}
		{#if renamedFrom}
			<p class="mt-4 rounded-md bg-surface-2 px-3 py-2 text-[0.8125rem] text-fg-muted">
				Renamed from <span class="font-mono">{repo.owner}/{renamedFrom}</span>. Old links redirect
				here.
			</p>
		{/if}

		<div class="mt-6">{@render children()}</div>
	{/if}
</main>
