<!--
	What changed since the last green run on this ref: the commits, the files
	they touched, and whether the run before this one failed too.
-->
<script lang="ts">
	import { RunState } from '#lib/api.ts';
	import { commitSubject, relativeTime, shortId, toDate } from '#lib/format.ts';
	import { RUN_LABEL } from '#lib/components/RunBadge.svelte';
	import { repoHref } from '#lib/data/forge.ts';
	import { CHANGE_LABEL, runHref, type RunPage } from '#lib/data/run.ts';

	let { data }: { data: RunPage } = $props();
	const run = $derived(data.run);
	const branch = $derived(run.ref.replace('refs/heads/', ''));
	const prev = $derived(
		data.history.find(
			(r) => Number(r.createTime?.seconds ?? 0n) < Number(run.createTime?.seconds ?? 0n)
		)
	);
</script>

<div class="grid gap-x-10 gap-y-4 text-[0.8125rem] lg:grid-cols-[minmax(0,1fr)_minmax(0,1fr)]">
	<div class="min-w-0">
		<p class="text-fg-muted">
			{#if run.state === RunState.SUCCEEDED}
				This run is green. Its commit:
			{:else if data.lastGreen}
				<span class="font-mono">{branch}</span> was green at
				<a href={runHref(data.lastGreen.id)} class="font-mono text-link hover:underline"
					>#{data.lastGreen.short}</a
				>, {relativeTime(toDate(data.lastGreen.createTime))}. {data.since.length} commit{data.since
					.length > 1
					? 's'
					: ''} since:
			{:else}
				No green run on <span class="font-mono">{branch}</span> in the last {data.history.length}
				runs.
			{/if}
		</p>
		<ul class="mt-2">
			{#each data.since as c (c.id)}
				<li>
					<a
						href={repoHref(run.repo, { at: c.id })}
						class="-mx-1 flex items-center gap-3 rounded-sm px-1 py-0.5 hover:bg-surface-2"
						><span class="font-mono text-xs text-link">{shortId(c.id)}</span><span
							class="min-w-0 flex-1 truncate">{commitSubject(c.message)}</span
						><span class="text-xs text-fg-subtle">{c.authorName}</span></a
					>
				</li>
			{/each}
		</ul>
		{#if prev && prev.id !== data.lastGreen?.id && run.state !== RunState.SUCCEEDED}
			<p class="mt-2 text-xs text-fg-subtle">
				The run before, <a href={runHref(prev.id)} class="font-mono hover:underline">#{prev.short}</a>,
				{RUN_LABEL[prev.state]} too.
			</p>
		{/if}
	</div>
	<div class="min-w-0">
		<p class="text-xs text-fg-subtle">
			{data.changes.length} file{data.changes.length === 1 ? '' : 's'} changed
		</p>
		<ul class="mt-2 space-y-0.5">
			{#each data.changes as c (`${c.kind}:${c.path}`)}
				<li class="flex items-center gap-3">
					<span class="w-16 shrink-0 text-xs text-fg-subtle">{CHANGE_LABEL[c.kind]}</span>
					<a
						href={repoHref(run.repo, { at: run.commitId, path: c.path })}
						class="min-w-0 truncate font-mono text-xs text-link hover:underline">{c.path}</a
					>
				</li>
			{/each}
		</ul>
	</div>
</div>
