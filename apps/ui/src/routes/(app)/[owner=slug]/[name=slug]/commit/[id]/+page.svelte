<!--
	/<owner>/<name>/commit/<id>: one commit and the files it changed.
	There is no GetCommit RPC: the commit comes from ListCommits(revision=id, 1).
	The unified text diff is a nice-to-have (needs a diff RPC).
-->
<script lang="ts">
	import { page } from '$app/state';
	import { ChangeKind, getCommitDiff, listCommits, ApiError, Code } from '#lib/api.ts';
	import CopyButton from '#lib/components/CopyButton.svelte';
	import Icon from '#lib/components/Icon.svelte';
	import Skeleton from '#lib/components/Skeleton.svelte';
	import StateView from '#lib/components/StateView.svelte';
	import { getRepoContext } from '#lib/context.ts';
	import {
		absoluteTime,
		commitBody,
		commitSubject,
		relativeTime,
		shortId,
		toDate,
		ZERO_ID
	} from '#lib/format.ts';
	import { commitHref, treeHref } from '#lib/repo-ui.ts';
	import { resource } from '#lib/resource.svelte.ts';

	const ctx = getRepoContext();

	const data = resource(async () => {
		const id = page.params.id!;
		const [log, diff] = await Promise.all([
			listCommits({ repo: ctx.key, revision: id, pageSize: 1 }),
			getCommitDiff({ repo: ctx.key, revision: id })
		]);
		const commit = log.commits[0];
		if (!commit) throw new ApiError(Code.NotFound, `no commit ${id}`);
		return { commit, changes: [...diff.changes].sort((a, b) => a.path.localeCompare(b.path)) };
	});

	const KIND = {
		[ChangeKind.UNSPECIFIED]: { letter: '?', label: 'changed', cls: 'bg-surface-2 text-fg-muted' },
		[ChangeKind.ADDED]: { letter: 'A', label: 'added', cls: 'bg-ok-soft text-ok' },
		[ChangeKind.MODIFIED]: { letter: 'M', label: 'modified', cls: 'bg-accent-soft text-accent' },
		[ChangeKind.DELETED]: { letter: 'D', label: 'deleted', cls: 'bg-danger-soft text-danger' },
		[ChangeKind.RENAMED]: { letter: 'R', label: 'renamed', cls: 'bg-warn-soft text-warn' }
	} as const;

	const c = $derived(data.data?.commit);
	const when = $derived(toDate(c?.commitTime));
	const counts = $derived.by(() => {
		const n = new Map<string, number>();
		for (const ch of data.data?.changes ?? []) {
			const l = KIND[ch.kind].label;
			n.set(l, (n.get(l) ?? 0) + 1);
		}
		return [...n].map(([l, k]) => `${k} ${l}`).join(', ');
	});
	const isZero = (id: string) => !id || id === ZERO_ID;
</script>

<svelte:head>
	<title>{c ? commitSubject(c.message) : 'Commit'} · {ctx.repo.owner}/{ctx.repo.name}</title>
</svelte:head>

{#if data.error}
	<StateView error={data.error} what="commit" onretry={data.retry} />
{:else if !c || !data.data}
	<div class="max-w-4xl rounded-md border border-border bg-surface p-5"><Skeleton lines={8} /></div>
{:else}
	<div class="max-w-4xl space-y-5">
		<article class="rounded-md border border-border bg-surface">
			<div class="px-5 pt-4 pb-3">
				<h2 class="text-base font-semibold">{commitSubject(c.message)}</h2>
				{#if commitBody(c.message)}
					<pre
						class="mt-2 font-sans text-[0.8125rem] whitespace-pre-wrap text-fg-muted">{commitBody(
							c.message
						)}</pre>
				{/if}
			</div>
			<dl
				class="grid grid-cols-[auto_minmax(0,1fr)] gap-x-5 gap-y-1.5 border-t border-border px-5 py-3 text-[0.8125rem]"
			>
				<dt class="text-fg-subtle">Author</dt>
				<dd>{c.authorName} <span class="text-fg-subtle">&lt;{c.authorEmail}&gt;</span></dd>
				<dt class="text-fg-subtle">Date</dt>
				<dd>
					<time datetime={when?.toISOString()}>{absoluteTime(when)}</time>
					<span class="text-fg-subtle">· {relativeTime(when)}</span>
				</dd>
				<dt class="text-fg-subtle">Commit</dt>
				<dd class="flex min-w-0 items-center gap-1">
					<span class="truncate font-mono text-xs">{c.id}</span>
					<CopyButton text={c.id} label="Copy commit id" class="p-1 text-fg-subtle hover:text-fg" />
				</dd>
				<dt class="text-fg-subtle">{c.parentIds.length > 1 ? 'Parents' : 'Parent'}</dt>
				<dd class="flex flex-wrap gap-x-3 font-mono text-xs leading-5">
					{#each c.parentIds as p (p)}
						<a href={commitHref(ctx.repo, p)} class="text-link hover:underline">{shortId(p)}</a>
					{:else}
						<span class="font-sans text-fg-subtle">none (root commit)</span>
					{/each}
				</dd>
			</dl>
			<div class="border-t border-border px-5 py-2.5">
				<a href={treeHref(ctx.repo, c.id)} class="text-[0.8125rem] text-link hover:underline"
					>Browse files at {shortId(c.id)} →</a
				>
			</div>
		</article>

		<section class="rounded-md border border-border bg-surface" aria-label="Changed files">
			<h3
				class="flex flex-wrap items-baseline gap-x-2 border-b border-border px-4 py-3 text-[0.8125rem]"
			>
				<span class="font-medium">{data.data.changes.length} files changed</span>
				<span class="text-fg-subtle">{counts}</span>
			</h3>
			<ul class="divide-y divide-border">
				{#each data.data.changes as ch (ch.path)}
					{@const k = KIND[ch.kind]}
					{@const at = ch.kind === ChangeKind.DELETED ? c.parentIds[0] : c.id}
					<li class="flex flex-wrap items-center gap-x-3 gap-y-1 px-4 py-2 text-[0.8125rem]">
						<span
							class="inline-flex size-5 shrink-0 items-center justify-center rounded-sm font-mono text-xs font-semibold {k.cls}"
							title={k.label}>{k.letter}<span class="sr-only"> {k.label}</span></span
						>
						{#if at}
							<a
								href={treeHref(ctx.repo, at, ch.path)}
								class="min-w-0 flex-1 truncate font-mono text-xs hover:text-link hover:underline {ch.kind ===
								ChangeKind.DELETED
									? 'line-through decoration-fg-subtle'
									: ''}">{ch.path}</a
							>
						{:else}
							<span class="min-w-0 flex-1 truncate font-mono text-xs">{ch.path}</span>
						{/if}
						<span class="font-mono text-[0.6875rem] text-fg-subtle" title="blob ids: old → new">
							{isZero(ch.oldId) ? '———————' : shortId(ch.oldId)} → {isZero(ch.newId)
								? '———————'
								: shortId(ch.newId)}
						</span>
					</li>
				{:else}
					<li class="px-4 py-4 text-[0.8125rem] text-fg-subtle">No file changes (empty commit).</li>
				{/each}
			</ul>
			<p
				class="flex items-center gap-1.5 border-t border-border px-4 py-2.5 text-xs text-fg-subtle"
			>
				<Icon name="file" size={12} /> Line-by-line diff: nice-to-have, not available from the API yet.
			</p>
		</section>
	</div>
{/if}
