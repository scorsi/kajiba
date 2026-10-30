<!--
	Repository switcher: shows the current repository; opens a filterable list.
	`/` anywhere (outside a text field) opens it; Enter goes to the first match.
-->
<script lang="ts">
	import { goto } from '$app/navigation';
	import { tick } from 'svelte';
	import type { Repo } from '#lib/api.ts';
	import { repoPath } from '#lib/repo-ui.ts';
	import Icon from './Icon.svelte';
	import Popover from './Popover.svelte';

	let { repos, current }: { repos: Repo[]; current?: Repo } = $props();

	let open = $state(false);
	let filter = $state('');
	let input = $state<HTMLInputElement>();

	const matches = $derived.by(() => {
		const q = filter.trim().toLowerCase();
		return q ? repos.filter((r) => `${r.owner}/${r.name}`.includes(q)) : repos;
	});
	const groups = $derived.by(() => {
		const map = new Map<string, Repo[]>();
		for (const r of matches) map.set(r.owner, [...(map.get(r.owner) ?? []), r]);
		return [...map];
	});

	$effect(() => {
		if (open) tick().then(() => input?.focus());
		else filter = '';
	});

	function onGlobalKey(e: KeyboardEvent) {
		if (e.key !== '/' || e.metaKey || e.ctrlKey || e.altKey) return;
		const t = e.target as HTMLElement;
		if (t.isContentEditable || ['INPUT', 'TEXTAREA', 'SELECT'].includes(t.tagName)) return;
		e.preventDefault();
		open = true;
	}

	function onFilterKey(e: KeyboardEvent) {
		if (e.key === 'Enter' && matches[0]) {
			open = false;
			goto(repoPath(matches[0]));
		}
	}
</script>

<svelte:window onkeydown={onGlobalKey} />

<Popover
	bind:open
	align="left"
	width="100%"
	class="w-full"
	label={current
		? `Repository ${current.owner}/${current.name}. Switch repository`
		: 'Choose a repository'}
	triggerClass="h-9 w-full gap-2 rounded-sm border border-border bg-bg px-2.5 text-[0.8125rem] hover:border-border-strong"
>
	{#snippet trigger()}
		{#if current}
			<span class="min-w-0 flex-1 truncate text-left">
				<span class="text-fg-muted">{current.owner} /</span>
				<span class="font-semibold">{current.name}</span>
			</span>
		{:else}
			<Icon name="search" size={14} class="text-fg-subtle" />
			<span class="flex-1 text-left text-fg-subtle">Go to repository</span>
		{/if}
		<kbd class="rounded-sm border border-border px-1 text-[0.6875rem] text-fg-subtle">/</kbd>
	{/snippet}

	<div class="border-b border-border p-2">
		<label class="relative block">
			<span class="sr-only">Filter repositories</span>
			<Icon
				name="search"
				size={14}
				class="pointer-events-none absolute top-2 left-2 text-fg-subtle"
			/>
			<input
				bind:this={input}
				bind:value={filter}
				onkeydown={onFilterKey}
				placeholder="Filter repositories"
				class="h-8 w-full rounded-sm bg-surface-2 pr-2 pl-7 text-[0.8125rem] placeholder:text-fg-subtle"
			/>
		</label>
	</div>
	<nav class="max-h-[60dvh] overflow-y-auto p-1" aria-label="Repositories">
		{#each groups as [owner, list] (owner)}
			<h2 class="px-2 pt-2 pb-1 text-xs font-medium text-fg-subtle">{owner}</h2>
			<ul>
				{#each list as r (r.publicId)}
					{@const active = r.publicId === current?.publicId}
					<li>
						<a
							href={repoPath(r)}
							onclick={() => (open = false)}
							aria-current={active ? 'page' : undefined}
							class="flex items-center gap-2 rounded-sm px-2 py-1 text-[0.8125rem] {active
								? 'bg-accent-soft font-medium text-accent'
								: r.archived
									? 'text-fg-subtle hover:bg-surface-2'
									: 'text-fg-muted hover:bg-surface-2 hover:text-fg'}"
						>
							<span class="truncate">{r.name}</span>
							{#if r.archived}<Icon
									name="archive"
									size={12}
									label="archived"
									class="ml-auto"
								/>{/if}
						</a>
					</li>
				{/each}
			</ul>
		{:else}
			<p class="px-2 py-2 text-[0.8125rem] text-fg-subtle">No repository matches “{filter}”.</p>
		{/each}
	</nav>
</Popover>
