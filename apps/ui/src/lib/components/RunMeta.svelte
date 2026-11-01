<!-- The facts of a run on one wrapping line: repository, ref, commit, author, trigger, runner, start. -->
<script lang="ts">
	import RunRef from '#lib/components/RunRef.svelte';
	import Icon from '#lib/components/Icon.svelte';
	import {
		absoluteTime,
		commitSubject,
		relativeTime,
		shortId,
		toDate
	} from '#lib/format.ts';
	import { repoHref } from '#lib/data/forge.ts';
	import type { RunPage } from '#lib/data/run.ts';

	let { data, class: cls = '' }: { data: RunPage; class?: string } = $props();
	const run = $derived(data.run);
</script>

<p class="flex flex-wrap items-center gap-x-3 gap-y-1 text-xs text-fg-subtle {cls}">
	<a href={repoHref(run.repo)} class="font-mono text-fg-muted hover:underline">{run.repo}</a>
	<RunRef {run} />
	<!-- a worktree run's commit is local: no link -->
	{#if run.trigger === 'worktree'}<span class="font-mono">{shortId(run.commitId)}</span>
	{:else}<a
			href={repoHref(run.repo, { at: run.commitId })}
			class="flex min-w-0 items-center gap-1.5 hover:underline"
			><span class="font-mono text-link">{shortId(run.commitId)}</span>{#if data.commit}<span
					class="truncate text-fg-muted">{commitSubject(data.commit.message)}</span
				>{/if}</a
		>{/if}
	{#if data.commit}<span>{data.commit.authorName}</span>{/if}
	{#if run.trigger !== 'worktree'}<span
		>{run.trigger === 'push' ? 'on push' : 'started manually'}{run.trigger !==
		'push'
			? run.triggeredBy
				? ` by ${run.triggeredBy}`
				: ''
			: ''}</span
	>
	{/if}
	{#if run.runner}<span class="flex items-center gap-1" title={run.runnerHostname}
			><Icon name="server" size={11} /><span class="font-mono">{run.runner}</span></span
		>{/if}
	<span title={absoluteTime(toDate(run.createTime))}>{relativeTime(toDate(run.createTime))}</span>
</p>
