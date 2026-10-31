<!--
	History: the event feed shown inside the History band, at forge scope
	(Activity page) or repository scope (repository view). Newest first, by day.
	Filters live in the URL: `kind` (see EVENT_FILTERS) and `in` (repository).
	The parent decides where links go (`commitHref`, `repoHref`), so the same
	feed time-travels inside a repository or jumps into one from the forge.
-->
<script lang="ts">
	import { goto } from '$app/navigation';
	import { page } from '$app/state';
	import { SvelteSet } from 'svelte/reactivity';
	import { RunState, type Run } from '#lib/api.ts';
	import Icon, { type IconName } from '#lib/components/Icon.svelte';
	import { commitSubject, relativeTime, shortId, toDate } from '#lib/format.ts';
	import RunBadge from '#lib/components/RunBadge.svelte';
	import {
		EVENT_FILTERS,
		countEvents,
		matches,
		parseFilter,
		type EventFilter,
		type HistoryEvent
	} from '#lib/data/events.ts';
	import { dayKey, dur } from '#lib/data/forge.ts';

	let {
		events,
		runByCommit,
		commitHref,
		repoHref,
		repos = [],
		current = '',
		groupLimit = 5,
		runHref = (id: string) => `/-/runs/${id}`
	}: {
		events: HistoryEvent[];
		runByCommit: Map<string, Run>;
		commitHref: (repo: string, commitId: string) => string;
		/** Given at forge scope: repository names are shown and linked. */
		repoHref?: (repo: string) => string;
		/** Repositories for the filter; the select is hidden with fewer than two. */
		repos?: string[];
		/** Commit id to highlight (time travel). */
		current?: string;
		/** Commits shown per group before "n more". */
		groupLimit?: number;
		runHref?: (id: string) => string;
	} = $props();

	const q = $derived(page.url.searchParams);
	const filter = $derived(parseFilter(q.get('kind')));
	const inRepo = $derived(repoHref ? (q.get('in') ?? '') : '');
	function hrefFor(next: Record<string, string | null>) {
		const u = new URLSearchParams(q.toString());
		for (const [k, v] of Object.entries(next)) v === null || v === '' ? u.delete(k) : u.set(k, v);
		return `?${u}`;
	}

	const scoped = $derived(inRepo ? events.filter((e) => matches(e, 'all', inRepo)) : events);
	const counts = $derived(countEvents(scoped));
	const countFor = (f: EventFilter) =>
		f === 'all'
			? undefined
			: f === 'commits'
				? counts.commits
				: f === 'release'
					? counts.release
					: f === 'ci'
						? counts.ci
						: counts.mirror + counts.runner;
	const visible = $derived(scoped.filter((e) => matches(e, filter)));
	const byDay = $derived.by(() => {
		const out: { k: string; dt: Date; items: HistoryEvent[] }[] = [];
		for (const e of visible) {
			const dt = new Date(e.t * 1000);
			const k = dayKey(dt);
			const last = out.at(-1);
			if (last?.k === k) last.items.push(e);
			else out.push({ k, dt, items: [e] });
		}
		return out;
	});

	const expanded = new SvelteSet<string>();
	const groupKey = (e: HistoryEvent & { kind: 'commits' }) => `${e.repo}|${e.author}|${e.t}`;

	const ICON: Record<HistoryEvent['kind'], IconName> = {
		commits: 'commit',
		release: 'tag',
		ci: 'play',
		mirror: 'mirror',
		runner: 'server'
	};
	const TONE: Record<HistoryEvent['kind'], string> = {
		commits: 'text-accent',
		release: 'text-warn',
		ci: 'text-danger',
		mirror: 'text-danger',
		runner: 'text-fg-subtle'
	};
	const dayFmt = new Intl.DateTimeFormat('en', {
		weekday: 'short',
		month: 'short',
		day: 'numeric'
	});
	const hm = new Intl.DateTimeFormat('en', { hour: '2-digit', minute: '2-digit', hour12: false });
	const todayKey = dayKey(new Date());
	const yesterdayKey = dayKey(new Date(new Date().setDate(new Date().getDate() - 1)));
	const dayLabel = (k: string, dt: Date) =>
		k === todayKey ? 'Today' : k === yesterdayKey ? 'Yesterday' : dayFmt.format(dt);
	const failedStep = (r: Run) => r.steps.find((s) => s.state === RunState.FAILED)?.name;
</script>

{#snippet repoName(repo: string)}
	{#if repoHref}<a href={repoHref(repo)} class="font-mono text-xs text-link hover:underline"
			>{repo}</a
		>{/if}
{/snippet}

<div
	class="sticky top-0 z-[2] flex flex-wrap items-center gap-2 border-b border-border/60 bg-surface px-4 py-2 sm:px-6"
>
	<nav class="flex flex-wrap gap-1 text-xs" aria-label="Event kind">
		{#each EVENT_FILTERS.filter((f) => f.id === 'all' || f.id === filter || countFor(f.id)) as f (f.id)}
			{@const n = countFor(f.id)}
			<a
				href={hrefFor({ kind: f.id === 'all' ? null : f.id })}
				data-sveltekit-noscroll
				aria-current={filter === f.id ? 'true' : undefined}
				class="rounded-full border px-2.5 py-0.5 {filter === f.id
					? 'border-accent bg-accent-soft text-accent'
					: 'border-border text-fg-muted hover:text-fg'}"
				>{f.label}{#if n !== undefined}<span class="tabular ml-1.5 text-fg-subtle">{n}</span
					>{/if}</a
			>
		{/each}
	</nav>
	{#if repoHref && repos.length > 1}
		<label class="ml-auto text-xs text-fg-subtle">
			<span class="sr-only">Repository</span>
			<select
				value={inRepo}
				onchange={(e) =>
					goto(hrefFor({ in: e.currentTarget.value }), { reset: false, replace: true })}
				class="h-6 rounded-sm border border-border bg-bg px-1.5 font-mono"
			>
				<option value="">all repositories</option>
				{#each repos as r (r)}<option value={r}>{r}</option>{/each}
			</select>
		</label>
	{/if}
</div>

<ol>
	{#each byDay as day (day.k)}
		<li
			class="grid grid-cols-[minmax(0,1fr)] border-b border-border/60 last:border-0 sm:grid-cols-[7rem_minmax(0,1fr)]"
		>
			<h3 class="px-4 pt-2 text-xs font-normal text-fg-subtle sm:py-2 sm:pr-2 sm:pl-6">
				{dayLabel(day.k, day.dt)}
			</h3>
			<ul>
				{#each day.items as e, i (i)}
					<li class="flex gap-2.5 py-1.5 pr-4 pl-4 text-[0.8125rem] sm:pr-6 sm:pl-0">
						<span class="w-10 shrink-0 pt-px font-mono text-xs text-fg-subtle"
							>{hm.format(new Date(e.t * 1000))}</span
						>
						<span class="shrink-0 pt-0.5 {TONE[e.kind]}" aria-hidden="true"
							><Icon name={ICON[e.kind]} size={13} /></span
						>
						<div class="min-w-0 flex-1">
							{#if e.kind === 'commits'}
								{@const key = groupKey(e)}
								{@const open = expanded.has(key)}
								{@const shown = open ? e.commits : e.commits.slice(0, groupLimit)}
								<p class="text-fg-muted">
									<span class="text-fg">{e.author}</span>
									pushed {e.commits.length} commit{e.commits.length > 1 ? 's' : ''}
									{#if repoHref}to {@render repoName(e.repo)}{/if}
								</p>
								<ul class="mt-0.5">
									{#each shown as c (c.id)}
										{@const run = runByCommit.get(c.id)}
										<li>
											<a
												href={commitHref(e.repo, c.id)}
												data-sveltekit-noscroll
												aria-current={c.id === current ? 'true' : undefined}
												class="-mx-1 flex items-center gap-2 rounded-sm px-1 py-0.5 hover:bg-surface-2 {c.id ===
												current
													? 'bg-accent-soft'
													: ''}"
												><span class="w-3 shrink-0"
													>{#if run}<RunBadge state={run.state} size={10} />{/if}</span
												><span class="w-14 shrink-0 font-mono text-xs text-fg-subtle"
													>{shortId(c.id)}</span
												><span class="min-w-0 flex-1 truncate">{commitSubject(c.message)}</span></a
											>
										</li>
									{/each}
								</ul>
								{#if e.commits.length > groupLimit}
									<button
										type="button"
										class="mt-0.5 pl-5 text-xs text-fg-subtle hover:text-fg"
										aria-expanded={open}
										onclick={() => (open ? expanded.delete(key) : expanded.add(key))}
										>{open ? 'show fewer' : `${e.commits.length - groupLimit} more`}</button
									>
								{/if}
							{:else if e.kind === 'release'}
								<p class="text-fg-muted">
									{#if repoHref}{@render repoName(e.repo)}{' '}{/if}released
									<a
										href={commitHref(e.repo, e.commit.id)}
										data-sveltekit-noscroll
										class="font-mono font-medium text-warn hover:underline">{e.name}</a
									>
									<span class="font-mono text-xs text-fg-subtle">{shortId(e.commit.id)}</span>
								</p>
							{:else if e.kind === 'ci'}
								<p class="text-fg-muted">
									Run <a href={runHref(e.run.id)} class="font-mono text-xs text-fg hover:underline"
										>#{e.run.short}</a
									>
									<span class="text-danger">failed</span>
									{#if repoHref}in {@render repoName(e.repo)}{/if}
								</p>
								<p class="text-xs text-fg-subtle">
									{#if failedStep(e.run)}at <span class="text-fg-muted">{failedStep(e.run)}</span
										>{' · '}{/if}{dur(e.run.durationSeconds)} · {e.run.runner} ·
									<a
										href={commitHref(e.repo, e.run.commitId)}
										data-sveltekit-noscroll
										class="font-mono hover:underline">{shortId(e.run.commitId)}</a
									>
								</p>
							{:else if e.kind === 'mirror'}
								<p class="text-fg-muted">
									Mirror {#if repoHref}of {@render repoName(e.repo)}{/if}
									<span class="text-danger">failing</span> · {e.mirror.attempts} attempts
								</p>
								<p class="text-xs break-words text-fg-subtle">
									<span class="font-mono">{e.mirror.lastError}</span>
								</p>
							{:else}
								<p class="text-fg-muted">
									Runner <span class="font-mono text-xs text-fg">{e.runner.name}</span> on {e.runner
										.host} went offline
									<span class="text-xs text-fg-subtle"
										>· {relativeTime(toDate(e.runner.lastSeenTime))}</span
									>
								</p>
							{/if}
						</div>
					</li>
				{/each}
			</ul>
		</li>
	{:else}
		<li class="px-4 py-3 text-[0.8125rem] text-fg-subtle sm:px-6">
			{events.length ? 'Nothing matches these filters.' : 'Nothing happened in this period.'}
		</li>
	{/each}
</ol>
