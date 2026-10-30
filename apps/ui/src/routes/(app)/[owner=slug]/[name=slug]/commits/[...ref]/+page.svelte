<!--
	/<owner>/<name>/commits/<ref>: history of a ref, newest first, grouped by
	day. Cursor pagination: `?after=<nextPageToken>&page=<n>` keeps every page
	linkable; "Newer" is browser history.
-->
<script lang="ts">
	import { goto } from '$app/navigation';
	import { page } from '$app/state';
	import { ApiError, Code, listCommits, type Commit } from '#lib/api.ts';
	import CommitRow from '#lib/components/CommitRow.svelte';
	import RefSelect from '#lib/components/RefSelect.svelte';
	import Skeleton from '#lib/components/Skeleton.svelte';
	import StateView from '#lib/components/StateView.svelte';
	import { getRepoContext } from '#lib/context.ts';
	import { refShortName, toDate } from '#lib/format.ts';
	import { commitsHref, splitRefPath } from '#lib/repo-ui.ts';
	import { resource } from '#lib/resource.svelte.ts';

	const PAGE_SIZE = 30;
	const ctx = getRepoContext();
	const target = $derived(splitRefPath(page.params.ref ?? '', ctx.refs));
	const after = $derived(page.url.searchParams.get('after') ?? '');
	const pageNo = $derived(Number(page.url.searchParams.get('page') ?? '1') || 1);

	const log = resource(async () => {
		const t = target;
		if (!t || t.path) throw new ApiError(Code.NotFound, `unknown ref '${page.params.ref}'`);
		return listCommits({
			repo: ctx.key,
			revision: t.revision,
			pageSize: PAGE_SIZE,
			pageToken: after
		});
	});

	const dayFmt = new Intl.DateTimeFormat('en', {
		weekday: 'short',
		year: 'numeric',
		month: 'short',
		day: 'numeric'
	});

	const days = $derived.by(() => {
		const groups: [string, Commit[]][] = [];
		for (const c of log.data?.commits ?? []) {
			const d = toDate(c.commitTime);
			const label = d ? dayFmt.format(d) : 'Unknown date';
			const last = groups.at(-1);
			if (last && last[0] === label) last[1].push(c);
			else groups.push([label, [c]]);
		}
		return groups;
	});

	const isCommitId = $derived(!!target && /^[0-9a-f]{40}$/.test(target.revision));
</script>

<svelte:head><title>History · {ctx.repo.owner}/{ctx.repo.name}</title></svelte:head>

<div class="max-w-4xl">
	<div class="mb-4 flex flex-wrap items-center gap-3">
		<h2 class="text-base font-semibold">History</h2>
		{#if target && !isCommitId}
			<RefSelect
				refs={ctx.refs}
				value={target.revision}
				onchange={(r) => goto(commitsHref(ctx.repo, r))}
				class="h-8 min-w-48 rounded-sm border border-border bg-surface"
			/>
		{:else if target}
			<span class="font-mono text-xs text-fg-muted">from {target.revision.slice(0, 7)}</span>
		{/if}
		{#if pageNo > 1}<span class="text-xs text-fg-subtle">page {pageNo}</span>{/if}
	</div>

	{#if log.error && !log.loading}
		<StateView error={log.error} what="ref" onretry={log.retry} />
	{:else if !log.data}
		<div class="rounded-md border border-border bg-surface p-4"><Skeleton lines={12} /></div>
	{:else if log.data.commits.length === 0}
		<p
			class="rounded-md border border-dashed border-border-strong px-4 py-8 text-center text-[0.8125rem] text-fg-muted"
		>
			No commits on {target ? refShortName(target.revision) : 'this ref'}.
		</p>
	{:else}
		<div class="space-y-5 transition-opacity {log.loading ? 'opacity-60' : ''}">
			{#each days as [day, commits] (day)}
				<section aria-label="Commits on {day}">
					<h3 class="mb-1.5 text-xs font-medium text-fg-subtle">{day}</h3>
					<ul class="divide-y divide-border rounded-md border border-border bg-surface">
						{#each commits as c (c.id)}<CommitRow repo={ctx.repo} commit={c} />{/each}
					</ul>
				</section>
			{/each}
		</div>

		<nav class="mt-5 flex items-center justify-between text-[0.8125rem]" aria-label="Pagination">
			{#if pageNo > 1}
				<span class="flex gap-4">
					<button type="button" onclick={() => history.back()} class="text-link hover:underline"
						>← Newer</button
					>
					<a href="?" class="text-fg-muted hover:underline">Newest</a>
				</span>
			{:else}<span></span>{/if}
			{#if log.data.nextPageToken}
				<a
					href="?after={encodeURIComponent(log.data.nextPageToken)}&page={pageNo + 1}"
					class="rounded-sm border border-border bg-surface px-3 py-1.5 hover:bg-surface-2"
					>Older →</a
				>
			{:else}
				<span class="text-xs text-fg-subtle">End of history</span>
			{/if}
		</nav>
	{/if}
</div>
