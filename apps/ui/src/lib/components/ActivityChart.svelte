<script lang="ts">
	import { RunState, type Commit } from '#lib/api.ts';
	import {
		activityBuckets,
		activityRange,
		aggregate,
		RANGES,
		successTrend,
		type ActivityBucket,
		type ActivitySource
	} from '#lib/data/activity.ts';
	import { dur } from '#lib/data/forge.ts';
	import { commitSubject, shortId, relativeTime, toDate } from '#lib/format.ts';
	import RunBadge, { RUN_LABEL } from '#lib/components/RunBadge.svelte';
	import RunRef from '#lib/components/RunRef.svelte';

	let {
		sources,
		range,
		selected = '',
		hrefFor,
		select,
		commitHref,
		byRef = false,
		listLabel = 'Pulse activity',
		currentCommit = '',
		cap
	}: {
		sources: ActivitySource[];
		range: number;
		selected?: string;
		hrefFor: (next: Record<string, string | null>) => string;
		select: (bucket: ActivityBucket) => void;
		commitHref: (repo: string, commit: Commit) => string;
		byRef?: boolean;
		listLabel?: string;
		currentCommit?: string;
		cap: string;
	} = $props();
	const days = $derived(activityRange(String(range)));
	const buckets = $derived(activityBuckets(sources, days));
	const max = $derived(Math.max(1, ...buckets.map((b) => b.pushes.length + b.runs.length)));
	const barMax = $derived(
		Math.max(1, ...buckets.map((b) => Math.max(b.runs.length, b.pushes.length)))
	);
	const picked = $derived(buckets.find((b) => b.k === selected));
	const recent = $derived(
		buckets
			.flatMap((b) => b.pushes.flatMap((p) => p.commits.map((c) => ({ c, repo: p.repo }))))
			.sort((a, b) => Number(b.c.commitTime?.seconds ?? 0n) - Number(a.c.commitTime?.seconds ?? 0n))
			.slice(0, 4)
	);
	const trend = $derived(successTrend(sources));
	const colours = ['bg-ok', 'bg-danger', 'bg-warn', 'bg-accent', 'bg-fg-subtle'];
	const labels = ['succeeded', 'failed', 'cancelled', 'running', 'queued'];
	const commits = (b: ActivityBucket) => b.pushes.reduce((n, p) => n + p.commits.length, 0);
	function description(b: ActivityBucket) {
		const a = aggregate(b, byRef);
		return (
			`${b.label}, ${commits(b)} commits · ${b.pushes.length} grouped pushes · ${b.runs.length} runs · ${b.releases.length} releases\n` +
			a.groups
				.map(
					(g) =>
						`${g.name}: ${g.commits} commits, ${g.runs} runs (${g.outcomes.map((n, i) => `${n} ${labels[i]}`).join(', ')})`
				)
				.join('\n') +
			`\nBranch/manual: ${a.branch.map((n, i) => `${n} ${labels[i]}`).join(', ')}` +
			`\nWorktree (lighter, separate stack): ${a.worktree.map((n, i) => `${n} ${labels[i]}`).join(', ')}` +
			`\nFinished runs: median ${a.median === undefined ? '—' : dur(a.median)}, longest ${a.longest === undefined ? '—' : dur(a.longest)}` +
			(b.releases.length ? `\n◆ ${b.releases.map((r) => `${r.repo} ${r.name}`).join(', ')}` : '')
		);
	}
</script>

{#snippet commitRow(repo: string, c: Commit)}
	{@const source = sources.find(s => s.repo === repo)}
	{@const run = source?.runs.find(r => r.trigger !== 'worktree' && r.commitId === c.id && (!r.ref || r.ref === source.ref))}
	<a href={commitHref(repo, c)} data-sveltekit-noscroll aria-current={currentCommit === c.id ? 'true' : undefined}
		class="flex items-center gap-3 rounded-sm px-1 py-0.5 hover:bg-surface-2 {currentCommit === c.id ? 'bg-surface-2' : ''}">
		<span class="w-3 shrink-0">{#if run}<RunBadge state={run.state} size={11} />{/if}</span>
		<span class="font-mono text-xs text-link">{shortId(c.id)}</span>
		{#if !byRef}<span class="font-mono text-xs text-fg-muted">{repo}</span>{/if}
		<span class="min-w-0 flex-1 truncate">{commitSubject(c.message)}</span>
		<span class="hidden shrink-0 text-xs text-fg-subtle sm:inline">{c.authorName} · {relativeTime(toDate(c.commitTime))}</span>
	</a>
{/snippet}

<nav class="mt-3 flex w-fit rounded-sm border border-border p-0.5 text-xs" aria-label="Period">
	{#each RANGES as r (r.days)}
		<a href={hrefFor({ days: String(r.days), day: null })} data-sveltekit-noscroll aria-current={r.days === days ? 'true' : undefined}
			class="rounded-[3px] px-2.5 py-1 {r.days === days ? 'bg-accent-soft text-accent' : 'text-fg-muted hover:text-fg'}">{r.label}</a>
	{/each}
</nav>
<figure class="mt-4" aria-label="Activity chart">
	<figcaption class="mb-2 text-xs text-fg-subtle">
		{days === 7 ? 'Pushes + runs by local hour · rows are days, columns 00–23' : days === 30 ? 'Daily runs by outcome · lighter stack: worktree · thin bar: grouped pushes · ◆ release' : 'Pushes + runs by week · oldest first, top to bottom'}
	</figcaption>
	{#if days === 30}
		<div class="flex h-20 items-end gap-1" data-granularity="day">
			{#each buckets as b (b.k)}
				{@const a = aggregate(b, byRef)}
				<button type="button" title={description(b)} aria-label={description(b)} aria-pressed={picked?.k === b.k} onclick={() => select(b)}
					class="daily-bar relative flex h-full min-w-0 flex-1 items-end gap-px border-b border-border hover:bg-surface-2">
					{#if b.releases.length}<span class="absolute -top-2 left-1/2 text-[10px] text-warn" aria-hidden="true">◆</span>{/if}
					{#each [a.branch, a.worktree] as outcomes, i (i)}
						<span class="flex h-full flex-1 flex-col justify-end" class:opacity-40={i === 1}>
							{#each outcomes as count, j (j)}
								<span class="block {colours[j]}" style:height="{count / barMax * 100}%"></span>
							{/each}
						</span>
					{/each}
					<span aria-hidden="true" class="absolute -bottom-3 left-0 w-full text-center text-[9px] text-fg-subtle">{commits(b) || ''}</span>
					<span class="w-1 bg-fg-subtle" style:height="{b.pushes.length / barMax * 100}%"></span>
				</button>
			{/each}
		</div>
		<svg class="mt-4 h-6 w-full text-accent" viewBox="0 0 300 24" preserveAspectRatio="none" role="img" aria-label="7-day rolling branch run success rate, succeeded divided by succeeded plus failed; gaps mean no finished runs">
			{#each trend as rate, i (i)}
				{#if rate !== null}
					<circle cx={i * 10 + 5} cy={22 - rate * 20} r="1" fill="currentColor"><title>{buckets[i].label}: {Math.round(rate * 100)}% success</title></circle>
					{#if i > 0 && trend[i - 1] !== null}<line x1={(i - 1) * 10 + 5} y1={22 - trend[i - 1]! * 20} x2={i * 10 + 5} y2={22 - rate * 20} stroke="currentColor" stroke-width="1" />{/if}
				{/if}
			{/each}
		</svg>
		<p class="text-xs text-fg-subtle">7-day rolling branch success rate (succeeded / succeeded + failed)</p>
		<p class="mt-1 text-xs text-fg-subtle">Succeeded · failed · cancelled · running · queued; hover or focus a bar for counts.</p>
	{:else}
		<div class:hour-frame={days === 7}>
			{#if days === 7}
				<div></div><div class="hour-axis" aria-hidden="true">{#each Array.from({length: 24}, (_, i) => i) as hour (hour)}<span>{String(hour).padStart(2, '0')}</span>{/each}</div>
				<div class="day-axis" aria-hidden="true">{#each buckets.filter((_, i) => i % 24 === 0) as b (b.k)}<span>{b.dt.toLocaleDateString(undefined, {month: 'short', day: 'numeric'})}</span>{/each}</div>
			{/if}
			<div class="heatmap" class:hours={days === 7} class:weeks={days === 365} data-granularity={days === 7 ? 'hour' : 'week'}>
				{#each buckets as b (b.k)}
					{@const n = b.pushes.length + b.runs.length}
					{@const failed = b.runs.some(r => r.state === RunState.FAILED)}
					<button type="button" title={description(b)} aria-label={description(b)} aria-pressed={picked?.k === b.k} onclick={() => select(b)}
						class="heat-cell {failed ? 'border-l-2 border-danger' : 'border border-border'}" style:background-color="color-mix(in srgb, var(--k-accent) {n ? 15 + n / max * 65 : 0}%, var(--k-surface-2))">
						<span aria-hidden="true">{failed ? '!' : n || '·'}</span>
					</button>
				{/each}
			</div>
		</div>
		<p class="mt-1 text-xs text-fg-subtle">Cell number = grouped pushes + runs · ! and red edge = failed run. {days === 7 ? 'Repeated DST hours share a cell; skipped hours are empty.' : ''}</p>
	{/if}
	<p class="mt-2 text-xs text-fg-subtle">{cap} Pushes are inferred per author/repository/hour from commit times; tags use their target commit time. Older activity may be absent.</p>
</figure>

{#if picked}<p class="mt-3 text-xs text-fg-muted">{picked.label} · {commits(picked)} commits · {picked.pushes.length} grouped pushes · {picked.runs.length} runs</p>{/if}
<ul aria-label={listLabel} class="mt-3 max-h-48 overflow-y-auto overscroll-contain space-y-0.5 text-[0.8125rem]">
	{#if picked}
		{#each picked.pushes as p, i (`${p.repo}/${i}`)}
			<li class="text-xs text-fg-subtle">Grouped push · {p.repo} · {p.author} · {p.commits.length} commits</li>
			{#each p.commits as c (`${p.repo}/${c.id}`)}
				<li>{@render commitRow(p.repo, c)}</li>
			{/each}
		{/each}
		{#each picked.runs as run (`${run.repo}/${run.id}`)}
			<li class="flex flex-wrap items-center gap-2"><a href={`/-/runs/${run.id}`} class="flex items-center gap-2 text-link"><RunBadge state={run.state} size={11} />{run.repo} #{run.short} · {RUN_LABEL[run.state]}</a><RunRef {run} /><span>{dur(run.durationSeconds)}</span></li>
		{/each}
		{#each picked.releases as r (`${r.repo}/${r.commit.id}/${r.name}`)}
			<li><a href={commitHref(r.repo, r.commit)} class="text-link">◆ {r.repo} {r.name}</a></li>
		{/each}
		{#if !picked.pushes.length && !picked.runs.length && !picked.releases.length}<li class="text-xs text-fg-subtle">No loaded activity in this bucket.</li>{/if}
	{:else}
		{#each recent as { c, repo } (`${repo}/${c.id}`)}
			<li>{@render commitRow(repo, c)}</li>
		{:else}
			<li class="text-xs text-fg-subtle">No loaded commits in this range.</li>
		{/each}
	{/if}
</ul>

<style>
	.hour-frame {
		display: grid;
		grid-template-columns: 3.5rem minmax(0, 1fr);
		gap: 3px;
	}
	.hour-axis {
		display: grid;
		grid-template-columns: repeat(24, minmax(0, 1fr));
		text-align: center;
		font-size: 9px;
		color: var(--k-fg-subtle);
	}
	.day-axis {
		display: grid;
		grid-template-rows: repeat(7, 1fr);
		align-items: center;
		font-size: 10px;
		color: var(--k-fg-subtle);
	}
	.heatmap {
		display: grid;
		gap: 3px;
	}
	.hours {
		grid-template-columns: repeat(24, minmax(0, 1fr));
	}
	.weeks {
		grid-template-rows: repeat(7, auto);
		grid-auto-flow: column;
		grid-auto-columns: minmax(0, 1fr);
	}
	.heat-cell {
		min-width: 0;
		min-height: 22px;
		border-radius: 2px;
		font-size: 10px;
		color: var(--k-fg);
	}
	.daily-bar[aria-pressed="true"],
	.heat-cell[aria-pressed="true"] {
		outline: 2px solid var(--k-focus);
	}
</style>
