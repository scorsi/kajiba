<!--
	Directory panel: ref switcher, latest commit, entries. Used by the repo home
	(root) and the tree pages (any directory; highlights the open file).
-->
<script lang="ts">
	import { goto } from '$app/navigation';
	import { EntryKind, type Commit, type Ref, type Repo, type TreeEntry } from '#lib/api.ts';
	import { absoluteTime, commitSubject, relativeTime, shortId, toDate } from '#lib/format.ts';
	import { ENTRY_ICON, commitHref, treeHref } from '#lib/repo-ui.ts';
	import Icon from './Icon.svelte';
	import RefSelect from './RefSelect.svelte';

	let {
		repo,
		refs,
		revision,
		path = '',
		entries,
		lastCommit,
		activeName,
		loading = false
	}: {
		repo: Repo;
		refs: Ref[];
		revision: string;
		path?: string;
		entries: TreeEntry[];
		lastCommit?: Commit;
		activeName?: string;
		loading?: boolean;
	} = $props();

	const when = $derived(toDate(lastCommit?.commitTime));
	const join = (name: string) => (path ? `${path}/${name}` : name);
	const parent = $derived(path.split('/').slice(0, -1).join('/'));
</script>

<section
	class="rounded-md border border-border bg-surface transition-opacity {loading
		? 'opacity-60'
		: ''}"
	aria-label="Files"
	aria-busy={loading}
>
	<div class="border-b border-border p-2">
		<RefSelect
			{refs}
			value={revision}
			onchange={(r) => goto(treeHref(repo, r, path))}
			class="h-7 w-full rounded-sm bg-surface-2"
		/>
	</div>
	{#if lastCommit}
		<a
			href={commitHref(repo, lastCommit.id)}
			class="block border-b border-border px-3 py-2 text-[0.8125rem] hover:bg-surface-2"
		>
			<span class="line-clamp-1">{commitSubject(lastCommit.message)}</span>
			<span class="text-xs text-fg-subtle">
				<span class="font-mono">{shortId(lastCommit.id)}</span> · {lastCommit.authorName} ·
				<time datetime={when?.toISOString()} title={absoluteTime(when)}>{relativeTime(when)}</time>
			</span>
		</a>
	{/if}
	<ul class="py-1">
		{#if path}
			<li>
				<a
					href={treeHref(repo, revision, parent)}
					class="flex items-center gap-2 px-3 py-1 text-[0.8125rem] text-fg-muted hover:bg-surface-2"
				>
					<Icon name="folder" size={15} class="text-fg-subtle" /> ..
					<span class="sr-only">parent directory</span>
				</a>
			</li>
		{/if}
		{#each entries as e (e.name)}
			<li>
				{#if e.kind === EntryKind.SUBMODULE}
					<span
						class="flex items-center gap-2 px-3 py-1 text-[0.8125rem] text-fg-muted"
						title="Submodule at {e.id}"
					>
						<Icon name={ENTRY_ICON[e.kind]} size={15} class="text-fg-subtle" />
						<span class="truncate">{e.name}</span>
						<span class="ml-auto font-mono text-xs text-fg-subtle">@{shortId(e.id)}</span>
					</span>
				{:else}
					{@const active = e.name === activeName}
					<a
						href={treeHref(repo, revision, join(e.name))}
						aria-current={active ? 'page' : undefined}
						class="flex items-center gap-2 px-3 py-1 text-[0.8125rem] {active
							? 'bg-accent-soft font-medium text-accent'
							: 'hover:bg-surface-2'}"
					>
						<Icon
							name={ENTRY_ICON[e.kind]}
							size={15}
							class={e.kind === EntryKind.TREE ? 'text-accent' : 'text-fg-subtle'}
						/>
						<span class="truncate">{e.name}</span>
						{#if e.kind === EntryKind.SYMLINK}<span class="ml-auto text-xs text-fg-subtle"
								>link</span
							>{/if}
					</a>
				{/if}
			</li>
		{:else}
			<li class="px-3 py-2 text-[0.8125rem] text-fg-subtle">Empty directory.</li>
		{/each}
	</ul>
</section>
