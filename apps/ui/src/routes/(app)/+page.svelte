<!-- Repository list: every repository visible to the viewer, grouped by owner. -->
<script lang="ts">
	import Icon from '#lib/components/Icon.svelte';
	import Skeleton from '#lib/components/Skeleton.svelte';
	import StateView from '#lib/components/StateView.svelte';
	import { refShortName } from '#lib/format.ts';
	import { allRepos } from '#lib/loaders.ts';
	import { repoPath } from '#lib/repo-ui.ts';
	import { resource } from '#lib/resource.svelte.ts';
	import { CiMode, type Repo } from '#lib/api.ts';

	const repos = resource(() => allRepos());
	let filter = $state('');
	let showArchived = $state(true);

	const groups = $derived.by(() => {
		const q = filter.trim().toLowerCase();
		const map = new Map<string, Repo[]>();
		for (const r of repos.data ?? []) {
			if (!showArchived && r.archived) continue;
			if (q && !`${r.owner}/${r.name} ${r.description}`.toLowerCase().includes(q)) continue;
			map.set(r.owner, [...(map.get(r.owner) ?? []), r]);
		}
		return [...map];
	});
	const archivedCount = $derived(repos.data?.filter((r) => r.archived).length ?? 0);
</script>

<svelte:head><title>Repositories · kajiba</title></svelte:head>

<main class="mx-auto max-w-5xl px-4 pt-5 pb-16 sm:px-6 lg:px-8 lg:pt-7">
	<header class="flex flex-wrap items-end gap-3">
		<div class="flex-1">
			<h1 class="text-xl font-semibold tracking-tight">Repositories</h1>
			{#if repos.data}
				<p class="text-[0.8125rem] text-fg-muted">
					{repos.data.length} visible to you{archivedCount ? `, ${archivedCount} archived` : ''}
				</p>
			{/if}
		</div>
		{#if repos.data?.length}
			<label class="relative w-full sm:w-64">
				<span class="sr-only">Filter repositories</span>
				<Icon
					name="search"
					size={14}
					class="pointer-events-none absolute top-2.5 left-2.5 text-fg-subtle"
				/>
				<input
					bind:value={filter}
					placeholder="Filter by name or description"
					class="h-9 w-full rounded-sm border border-border bg-surface pr-2 pl-8 text-[0.8125rem] placeholder:text-fg-subtle"
				/>
			</label>
			{#if archivedCount}
				<label class="flex h-9 items-center gap-2 text-[0.8125rem] text-fg-muted">
					<input type="checkbox" bind:checked={showArchived} class="accent-(--k-accent)" />
					Show archived
				</label>
			{/if}
		{/if}
	</header>

	<div class="mt-6">
		{#if repos.error}
			<StateView error={repos.error} onretry={repos.retry} />
		{:else if !repos.data}
			<div class="rounded-md border border-border bg-surface p-4"><Skeleton lines={8} /></div>
		{:else if repos.data.length === 0}
			<div class="rounded-md border border-dashed border-border-strong px-6 py-12 text-center">
				<Icon name="folder" size={28} class="mx-auto text-fg-subtle" />
				<p class="mt-3 font-medium">No repositories yet</p>
				<p class="mt-1 text-[0.8125rem] text-fg-muted">
					Repositories are declared in the server configuration. None is visible to you.
				</p>
			</div>
		{:else}
			{#each groups as [owner, list] (owner)}
				<section class="mb-6">
					<h2 class="mb-2 text-[0.8125rem] font-medium text-fg-subtle">{owner}</h2>
					<ul class="divide-y divide-border rounded-md border border-border bg-surface">
						{#each list as r (r.publicId)}
							<li>
								<a
									href={repoPath(r)}
									class="flex flex-wrap items-baseline gap-x-3 gap-y-1 px-4 py-3 hover:bg-surface-2"
								>
									<span class="font-medium {r.archived ? 'text-fg-muted' : ''}">{r.name}</span>
									{#if r.archived}
										<span
											class="inline-flex items-center gap-1 rounded-full bg-warn-soft px-1.5 text-xs text-warn"
											><Icon name="archive" size={11} />archived</span
										>
									{/if}
									<span
										class="min-w-0 flex-1 basis-full truncate text-[0.8125rem] text-fg-muted sm:basis-0"
									>
										{r.description || '—'}
									</span>
									<span class="flex items-center gap-3 text-xs text-fg-subtle">
										{#if r.ciMode > CiMode.OFF}<span class="flex items-center gap-1" title="CI enabled"
												><Icon name="play" size={12} />CI</span
											>{/if}
										{#if r.mirrors.some((m) => m.enabled)}<span
												class="flex items-center gap-1"
												title="Push-mirrored"><Icon name="mirror" size={12} />mirror</span
											>{/if}
										<span class="font-mono">{refShortName(r.defaultBranch)}</span>
									</span>
								</a>
							</li>
						{/each}
					</ul>
				</section>
			{:else}
				<p class="text-[0.8125rem] text-fg-subtle">No repository matches “{filter}”.</p>
			{/each}
		{/if}
	</div>
</main>
