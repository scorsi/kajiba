<!-- Path breadcrumbs inside a repository: repo / dir / dir / file. -->
<script lang="ts">
	import type { Repo } from '#lib/api.ts';
	import { treeHref } from '#lib/repo-ui.ts';

	let { repo, revision, path }: { repo: Repo; revision: string; path: string } = $props();
	const parts = $derived(path.split('/').filter(Boolean));
</script>

<nav aria-label="Path" class="min-w-0">
	<ol class="flex flex-wrap items-center gap-x-1 font-mono text-[0.8125rem]">
		<li><a href={treeHref(repo, revision)} class="text-link hover:underline">{repo.name}</a></li>
		{#each parts as part, i (i)}
			<li class="flex items-center gap-x-1">
				<span class="text-fg-subtle" aria-hidden="true">/</span>
				{#if i === parts.length - 1}
					<span aria-current="page" class="font-medium">{part}</span>
				{:else}
					<a
						href={treeHref(repo, revision, parts.slice(0, i + 1).join('/'))}
						class="text-link hover:underline">{part}</a
					>
				{/if}
			</li>
		{/each}
	</ol>
</nav>
