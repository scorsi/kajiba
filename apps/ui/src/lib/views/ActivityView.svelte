<!--
	Activity model A — "Bands": the repository view's grammar at forge scale.
	Pulse (forge-wide ribbon, state line, what needs attention, latest
	commits), Pipelines (running now, recent failures), History (folded: the
	forge's events — commits, releases, failed runs, mirrors, runners), then
	every repository as a small card. Same folding strips as the repository view.
-->
<script lang="ts">
	import { page } from '$app/state';
	import type { Snippet } from 'svelte';
	import { MirrorState, RunState, type Commit } from '#lib/api.ts';
	import Icon from '#lib/components/Icon.svelte';
	import { commitSubject, relativeTime, shortId, toDate } from '#lib/format.ts';
	import RunBadge, { RUN_LABEL } from '#lib/components/RunBadge.svelte';
	import { buildEvents, countEvents } from '#lib/data/events.ts';
	import { commitDay, dur, lastDays, repoHref, type Forge } from '#lib/data/forge.ts';
	import HistoryFeed from '#lib/components/HistoryFeed.svelte';
	import NeedsAttention from '#lib/components/NeedsAttention.svelte';

	let { forge }: { forge: Forge } = $props();

	const q = $derived(page.url.searchParams);
	const days = $derived(q.get('days') === '30' ? 30 : 90);
	const pipelinesOpen = $derived(q.get('pipelines') !== '0');
	const historyOpen = $derived(q.get('history') === '1');
	function hrefFor(next: Record<string, string | null>) {
		const u = new URLSearchParams(q.toString());
		for (const [k, v] of Object.entries(next)) v === null ? u.delete(k) : u.set(k, v);
		return `?${u}`;
	}

	const win = $derived(lastDays(days));
	const since = $derived(win[0].k);
	const all = $derived(
		forge.repos
			.flatMap((r) => r.commits.map((c) => ({ c, r })))
			.sort((a, b) => Number(b.c.commitTime!.seconds - a.c.commitTime!.seconds))
	);
	const inWindow = $derived(all.filter((x) => commitDay(x.c) >= since));
	const runByCommit = $derived(new Map(forge.runs.map((r) => [r.commitId, r])));

	const ribbon = $derived.by(() => {
		const n = new Map<string, number>();
		const rel = new Map<string, string[]>();
		const ci = new Map<string, RunState[]>();
		for (const { c, r } of inWindow) {
			const k = commitDay(c);
			n.set(k, (n.get(k) ?? 0) + 1);
			const t = r.tags.get(c.id);
			if (t) rel.set(k, [...(rel.get(k) ?? []), ...t.map((x) => `${r.repo.name} ${x}`)]);
			const run = runByCommit.get(c.id);
			if (run) ci.set(k, [...(ci.get(k) ?? []), run.state]);
		}
		return win.map((d) => {
			const s = ci.get(d.k) ?? [];
			const worst = [
				RunState.RUNNING,
				RunState.FAILED,
				RunState.CANCELLED,
				RunState.SUCCEEDED
			].find((x) => s.includes(x));
			return { ...d, n: n.get(d.k) ?? 0, releases: rel.get(d.k) ?? [], ci: worst };
		});
	});
	const max = $derived(Math.max(1, ...ribbon.map((d) => d.n)));

	const weekAgo = Date.now() / 1000 - 7 * 86400;
	const runs7 = $derived(forge.runs.filter((r) => Number(r.createTime!.seconds) > weekAgo));
	const passed7 = $derived(runs7.filter((r) => r.state === RunState.SUCCEEDED).length);
	const running = $derived(forge.runs.filter((r) => r.state === RunState.RUNNING));
	const failures = $derived(runs7.filter((r) => r.state === RunState.FAILED));
	const activeRepos = $derived(new Set(inWindow.map((x) => x.r.key)).size);
	const authors = $derived(new Set(inWindow.map((x) => x.c.authorName)).size);
	const mirrorsOk = $derived(forge.mirrors.filter((m) => m.state === MirrorState.OK).length);
	const mirrorsFailing = $derived(forge.mirrors.filter((m) => m.state === MirrorState.FAILING));

	const cards = $derived(
		[...forge.repos].sort(
			(a, b) =>
				Number(b.commits[0]?.commitTime?.seconds ?? 0n) -
				Number(a.commits[0]?.commitTime?.seconds ?? 0n)
		)
	);
	const spark = (commits: Commit[]) => {
		const w = lastDays(30);
		const n = new Map<string, number>();
		for (const c of commits) n.set(commitDay(c), (n.get(commitDay(c)) ?? 0) + 1);
		const v = w.map((d) => n.get(d.k) ?? 0);
		return { v, max: Math.max(1, ...v) };
	};
	const fullDay = new Intl.DateTimeFormat('en', {
		weekday: 'short',
		month: 'short',
		day: 'numeric'
	});
	// History: the forge's events over the Pulse period
	const events = $derived(
		buildEvents(
			forge.repos.map((r) => ({ ...r, repo: r.key })),
			{
				mirrors: forge.mirrors,
				runners: forge.runners,
				since: new Date(win[0].dt).setHours(0, 0, 0, 0) / 1000
			}
		)
	);
	const counts = $derived(countEvents(events));
</script>

{#snippet strip(
	label: string,
	open: boolean,
	param: string,
	openV: string | null,
	closedV: string | null,
	summary: Snippet
)}
	<a
		href={hrefFor({ [param]: open ? closedV : openV })}
		data-sveltekit-noscroll
		aria-expanded={open}
		class="flex h-8 shrink-0 items-center gap-3 border-b border-border px-4 text-xs text-fg-muted hover:bg-surface-2 sm:px-6"
	>
		<span class="w-3 text-fg-subtle" aria-hidden="true">{open ? '▾' : '▸'}</span>
		<span class="tracking-wide text-fg-subtle uppercase">{label}</span>
		<span class="flex min-w-0 flex-1 items-center gap-3 truncate">{@render summary()}</span>
	</a>
{/snippet}

<div class="relative min-h-0 flex-1 overflow-y-auto">
	<!-- Pulse -->
	<section class="border-b border-border bg-surface px-4 pt-4 pb-4 sm:px-6" aria-label="Pulse">
		<p class="text-xs tracking-wide text-fg-subtle uppercase">▾ Pulse</p>
		<div class="mt-1 flex flex-wrap items-end gap-3">
			<h1 class="flex-1 text-2xl font-semibold tracking-tight">
				Activity <span class="text-base font-normal text-fg-muted"
					>across {forge.repos.length} repositories</span
				>
			</h1>
			<nav class="flex rounded-sm border border-border p-0.5 text-xs" aria-label="Period">
				{#each [30, 90] as d (d)}
					<a
						href={hrefFor({ days: d === 90 ? null : '30' })}
						data-sveltekit-noscroll
						aria-current={d === days ? 'true' : undefined}
						class="rounded-[3px] px-2.5 py-1 {d === days
							? 'bg-accent-soft text-accent'
							: 'text-fg-muted hover:text-fg'}">{d} days</a
					>
				{/each}
			</nav>
		</div>
		<p class="mt-2 flex flex-wrap items-center gap-x-4 gap-y-1 text-xs text-fg-subtle">
			<span><span class="tabular text-fg-muted">{inWindow.length}</span> commits</span>
			<span><span class="tabular text-fg-muted">{activeRepos}</span> active repositories</span>
			<span><span class="tabular text-fg-muted">{authors}</span> authors</span>
			<span class="flex items-center gap-1.5"
				><RunBadge state={RunState.SUCCEEDED} size={10} />CI {passed7}/{runs7.length} passed · 7 days</span
			>
			{#if running.length}<span class="flex items-center gap-1.5"
					><RunBadge state={RunState.RUNNING} size={10} />{running.length} running</span
				>{/if}
			<span class="flex items-center gap-1.5 {mirrorsFailing.length ? 'text-danger' : ''}"
				><Icon name="mirror" size={11} />mirrors {mirrorsOk} ok{mirrorsFailing.length
					? ` · ${mirrorsFailing.length} failing`
					: ''}</span
			>
		</p>
		<NeedsAttention {forge} />
		<figure class="mt-4">
			<figcaption class="mb-1.5 flex justify-between text-xs text-fg-subtle">
				<span>Commits per day, all repositories</span><span>◆ release</span>
			</figcaption>
			<div class="flex h-14 items-end gap-[2px]">
				{#each ribbon as d (d.k)}
					<div
						class="relative flex h-full flex-1 items-end"
						title="{fullDay.format(d.dt)} · {d.n} commits{d.releases.length
							? ` · ${d.releases.join(', ')}`
							: ''}{d.ci !== undefined ? ` · CI ${RUN_LABEL[d.ci]}` : ''}"
					>
						{#if d.releases.length}<span
								class="absolute -top-1.5 left-1/2 size-1.5 -translate-x-1/2 rotate-45 bg-warn"
								aria-hidden="true"
							></span>{/if}
						<span
							class="block w-full rounded-t-[2px] {d.n ? 'bg-accent/75' : 'bg-border/70'}"
							style:height="{d.n ? Math.max(12, (d.n / max) * 100) : 5}%"
						></span>
					</div>
				{/each}
			</div>
			<div class="mt-1 flex gap-[2px]" role="img" aria-label="CI outcome per day">
				{#each ribbon as d (d.k)}
					<span
						class="h-1 flex-1 rounded-full {d.ci === RunState.FAILED
							? 'bg-danger'
							: d.ci === RunState.RUNNING
								? 'bg-accent'
								: d.ci === RunState.SUCCEEDED
									? 'bg-ok/60'
									: 'bg-transparent'}"
					></span>
				{/each}
			</div>
		</figure>
		<ul class="mt-3 space-y-0.5 text-[0.8125rem]">
			{#each inWindow.slice(0, 4) as { c, r } (`${r.key}/${c.id}`)}
				{@const run = runByCommit.get(c.id)}
				<li>
					<a
						href={repoHref(r.key, { at: c.id })}
						class="flex items-center gap-3 rounded-sm px-1 py-0.5 hover:bg-surface-2"
					>
						<span class="w-3"
							>{#if run}<RunBadge state={run.state} size={11} />{/if}</span
						>
						<span class="w-32 shrink-0 truncate font-mono text-xs text-fg-muted">{r.key}</span>
						<span class="min-w-0 flex-1 truncate">{commitSubject(c.message)}</span>
						<span class="hidden shrink-0 text-xs text-fg-subtle sm:inline"
							>{c.authorName} · {relativeTime(toDate(c.commitTime))}</span
						>
					</a>
				</li>
			{/each}
		</ul>
	</section>

	<!-- Pipelines -->
	{#snippet pipeSummary()}
		{#each running as r (r.id)}<span class="flex items-center gap-1.5"
				><RunBadge state={r.state} size={10} /><span class="text-fg-muted">{r.repo} #{r.short}</span
				></span
			>{/each}
		<span class={failures.length ? 'text-danger' : ''}>{failures.length} failed · 7 days</span>
	{/snippet}
	{@render strip('Pipelines', pipelinesOpen, 'pipelines', null, '0', pipeSummary)}
	{#if pipelinesOpen}
		<section class="border-b border-border bg-surface px-4 py-3 sm:px-6" aria-label="Pipelines">
			<div class="grid grid-cols-[minmax(0,1fr)] gap-4 lg:grid-cols-2">
				<div>
					<h2 class="mb-1.5 text-xs text-fg-subtle">Running now</h2>
					{#each running as r (r.id)}
						<a
							href="/-/runs/{r.id}"
							class="flex items-center gap-2 rounded-md border border-border bg-bg px-3 py-2 text-[0.8125rem] hover:border-border-strong"
						>
							<RunBadge state={r.state} label />
							<span class="font-mono text-xs text-fg-muted">{r.repo} #{r.short}</span>
							<span class="ml-auto flex w-32 gap-0.5" aria-hidden="true"
								>{#each r.steps as s, si (si)}<span
										class="h-1 flex-1 rounded-full {s.state === RunState.SUCCEEDED
											? 'bg-ok'
											: s.state === RunState.RUNNING
												? 'bg-accent'
												: 'bg-border'}"
									></span>{/each}</span
							>
							<span class="tabular text-xs text-fg-subtle">{dur(r.durationSeconds)}</span>
						</a>
					{:else}
						<p class="text-[0.8125rem] text-fg-subtle">Nothing running.</p>
					{/each}
				</div>
				<div>
					<h2 class="mb-1.5 text-xs text-fg-subtle">Failures · 7 days</h2>
					<ul class="space-y-0.5 text-[0.8125rem]">
						{#each failures.slice(0, 4) as r (r.id)}
							<li>
								<a
									href="/-/runs/{r.id}"
									class="flex items-center gap-2 rounded-sm px-1 py-0.5 hover:bg-surface-2"
									><RunBadge state={r.state} size={11} /><span
										class="font-mono text-xs text-fg-muted">{r.repo} #{r.short}</span
									><span class="truncate text-xs text-fg-subtle"
										>{r.steps.find((s) => s.state === RunState.FAILED)?.name}</span
									><span class="ml-auto text-xs text-fg-subtle"
										>{relativeTime(toDate(r.createTime))}</span
									></a
								>
							</li>
						{:else}
							<li class="text-fg-subtle">No failure.</li>
						{/each}
					</ul>
				</div>
			</div>
		</section>
	{/if}

	<!-- History -->
	{#snippet historySummary()}
		<span class="tabular">{counts.commits} commits</span>
		{#if counts.release}<span class="tabular text-warn"
				>{counts.release} release{counts.release > 1 ? 's' : ''}</span
			>{/if}
		{#if counts.ci}<span class="tabular text-danger"
				>{counts.ci} failed run{counts.ci > 1 ? 's' : ''}</span
			>{/if}
		{#if counts.mirror}<span class="tabular text-danger"
				>{counts.mirror} mirror{counts.mirror > 1 ? 's' : ''} failing</span
			>{/if}
		{#if counts.runner}<span class="tabular"
				>{counts.runner} runner{counts.runner > 1 ? 's' : ''} offline</span
			>{/if}
	{/snippet}
	{@render strip('History', historyOpen, 'history', '1', null, historySummary)}
	{#if historyOpen}
		<section
			class="relative max-h-[55vh] overflow-y-auto border-b border-border bg-surface"
			aria-label="History"
		>
			<HistoryFeed
				{events}
				{runByCommit}
				commitHref={(repo, id) => repoHref(repo, { at: id, history: '1' })}
				repoHref={(repo) => repoHref(repo)}
				repos={forge.repos.map((r) => r.key)}
				groupLimit={4}
			/>
		</section>
	{/if}

	<!-- Repositories -->
	<section class="px-4 py-5 sm:px-6" aria-label="Repositories">
		<h2 class="mb-3 text-xs tracking-wide text-fg-subtle uppercase">Repositories</h2>
		<ul class="grid gap-3 sm:grid-cols-2 xl:grid-cols-3">
			{#each cards as r (r.key)}
				{@const s = spark(r.commits)}
				{@const last = r.commits[0]}
				{@const run = r.runs[0]}
				<li>
					<a
						href={repoHref(r.key)}
						class="block h-full rounded-md border border-border bg-surface px-4 py-3 hover:border-border-strong"
					>
						<div class="flex items-center gap-2">
							<span class="font-mono text-[0.8125rem] {r.repo.archived ? 'text-fg-subtle' : ''}"
								>{r.key}</span
							>
							{#if r.repo.archived}<span class="text-xs text-warn">archived</span>{/if}
							<span class="ml-auto flex items-center gap-2">
								{#if r.mirror?.state === MirrorState.FAILING}<span
										class="flex items-center gap-1 text-xs text-danger"
										><Icon name="mirror" size={11} />failing</span
									>{/if}
								{#if run}<RunBadge state={run.state} size={12} />{/if}
							</span>
						</div>
						<div class="mt-2 flex h-6 items-end gap-px" aria-hidden="true">
							{#each s.v as v, i (i)}<span
									class="flex-1 rounded-[1px] {v ? 'bg-accent/70' : 'bg-border/80'}"
									style:height="{v ? Math.max(20, (v / s.max) * 100) : 10}%"
								></span>{/each}
						</div>
						<p class="mt-2 truncate text-xs text-fg-muted">
							{last ? commitSubject(last.message) : 'Empty repository'}
						</p>
						<p class="text-xs text-fg-subtle">
							{last
								? `${last.authorName} · ${relativeTime(toDate(last.commitTime))} · ${shortId(last.id)}`
								: '—'}
						</p>
					</a>
				</li>
			{/each}
		</ul>
	</section>
</div>
