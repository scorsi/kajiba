<!-- Repository home: files at the default branch, details, README. -->
<script lang="ts">
	import EmptyRepo from '#lib/components/EmptyRepo.svelte';
	import FileList from '#lib/components/FileList.svelte';
	import Markdown from '#lib/components/Markdown.svelte';
	import RepoDetails from '#lib/components/RepoDetails.svelte';
	import Skeleton from '#lib/components/Skeleton.svelte';
	import StateView from '#lib/components/StateView.svelte';
	import { getRepoContext } from '#lib/context.ts';
	import { refShortName } from '#lib/format.ts';
	import { loadDirectory } from '#lib/loaders.ts';
	import { resource } from '#lib/resource.svelte.ts';
	import { treeHref } from '#lib/repo-ui.ts';

	const ctx = getRepoContext();
	const empty = $derived(!ctx.refs.some((r) => r.name === ctx.repo.defaultBranch));
	const dir = resource(() =>
		empty ? Promise.resolve(undefined) : loadDirectory(ctx.key, ctx.repo.defaultBranch)
	);
</script>

{#if empty}
	<EmptyRepo repo={ctx.repo} />
{:else if dir.error}
	<StateView error={dir.error} onretry={dir.retry} />
{:else}
	<div class="grid gap-5 xl:grid-cols-[22rem_minmax(0,1fr)]">
		<div class="min-w-0 space-y-5">
			{#if dir.data}
				<FileList
					repo={ctx.repo}
					refs={ctx.refs}
					revision={ctx.repo.defaultBranch}
					entries={dir.data.entries}
					lastCommit={dir.data.lastCommit}
				/>
			{:else}
				<div class="rounded-md border border-border bg-surface p-3"><Skeleton lines={10} /></div>
			{/if}
			<RepoDetails repo={ctx.repo} />
		</div>

		{#if !dir.data}
			<div class="rounded-md border border-border bg-surface p-7"><Skeleton lines={8} /></div>
		{:else if dir.data.readme}
			<article class="min-w-0 rounded-md border border-border bg-surface">
				<h2 class="border-b border-border px-5 py-2.5 text-[0.8125rem] font-medium text-fg-muted">
					{dir.data.readme.name}
				</h2>
				<Markdown
					source={dir.data.readme.source}
					linkBase={treeHref(ctx.repo, ctx.repo.defaultBranch) + '/'}
					class="p-5 sm:p-7"
				/>
			</article>
		{:else}
			<p class="text-[0.8125rem] text-fg-subtle">
				No README at the root of {refShortName(ctx.repo.defaultBranch)}.
			</p>
		{/if}
	</div>
{/if}
