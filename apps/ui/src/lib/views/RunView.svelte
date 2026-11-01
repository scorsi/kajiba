<!--
	Run model A — "Bands": the run in the same grammar as the repository view.
	Pulse (state, facts, a step timeline, the runs before it on the same ref),
	Steps as horizontal columns (StepColumns: the step in focus shows its
	console, flake checks as nested columns, spines with duration against
	usual), with What changed since the last green run above them.
	URL: ?step, ?check (StepColumns), ?steps=0 folds Steps, ?changed=0|1.
-->
<script lang="ts">
	import RunActions from '#lib/components/RunActions.svelte';
	import RunRef from '#lib/components/RunRef.svelte';
	import { page } from '$app/state';
	import { RunState } from '#lib/api.ts';
	import { relativeTime, toDate } from '#lib/format.ts';
	import RunBadge, { RUN_LABEL } from '#lib/components/RunBadge.svelte';
	import BandStrip from '#lib/components/BandStrip.svelte';
	import RunMeta from '#lib/components/RunMeta.svelte';
	import StepColumns from '#lib/components/StepColumns.svelte';
	import WhatChanged from '#lib/components/WhatChanged.svelte';
	import { clock, runHref, type RunPage } from '#lib/data/run.ts';

	// Why a queued run waits, in words (ADR 0109).
	const PENDING: Record<string, string> = {
		'no-connected-runner': 'Waiting: no runner is connected.',
		'backend-unavailable': 'Waiting: no connected runner offers the backend this run needs.',
		'tags-unmatched': 'Waiting: no runner has the tags this run requires.',
		'runners-busy': 'Waiting: every matching runner is busy.'
	};

	let { data, onchange }: { data: RunPage; onchange: () => void } = $props();
	const run = $derived(data.run);

	const q = $derived(page.url.searchParams);
	const pulseOpen = $derived(q.get('pulse') !== '0');
	function hrefFor(next: Record<string, string | null>) {
		const u = new URLSearchParams(q.toString());
		for (const [k, v] of Object.entries(next)) v === null ? u.delete(k) : u.set(k, v);
		return `?${u}`;
	}
	const stepHref = (i: number) => hrefFor({ step: String(i + 1), check: null, steps: null });
	const stepsOpen = $derived(q.get('steps') !== '0');
	const changedOpen = $derived(q.get('changed') === '1');
	const changedHref = $derived(hrefFor({ changed: changedOpen ? null : '1' }));

	const running = $derived(run.state === RunState.RUNNING);
	const elapsedOf = (i: number) => {
		const s = run.steps[i];
		if (s.state !== RunState.RUNNING) return s.durationSeconds;
		const before = run.steps.slice(0, i).reduce((a, x) => a + x.durationSeconds, 0);
		return Math.max(0, run.durationSeconds - before);
	};
	/** Timeline widths: real durations; a running or queued step takes its usual time. */
	const widths = $derived.by(() => {
		const w = run.steps.map((s, i) =>
			s.state === RunState.RUNNING || s.state === RunState.QUEUED
				? Math.max(elapsedOf(i), data.usual[i] || 30)
				: Math.max(s.durationSeconds, 12)
		);
		const total = w.reduce((a, b) => a + b, 0) || 1;
		return w.map((x) => (x / total) * 100);
	});

	const older = $derived([...data.history].reverse().slice(-24));
	const STATE_BG: Record<RunState, string> = {
		[RunState.UNSPECIFIED]: 'bg-border',
		[RunState.QUEUED]: 'bg-border',
		[RunState.RUNNING]: 'bg-accent',
		[RunState.SUCCEEDED]: 'bg-ok',
		[RunState.FAILED]: 'bg-danger',
		[RunState.CANCELLED]: 'bg-fg-subtle/50'
	};
</script>

<div class="relative flex min-h-0 flex-1 flex-col overflow-y-auto [&>*]:shrink-0">
	<!-- Pulse -->
	<BandStrip label="Pulse" open={pulseOpen} href={hrefFor({ pulse: pulseOpen ? '0' : null })}>
		<span>Run #{run.short}</span>{#if run.trigger === 'worktree'}<RunRef {run} />{/if}
	</BandStrip>
	{#if pulseOpen}
	<section class="border-b border-border bg-surface px-4 py-4 sm:px-6" aria-label="Pulse">
		<div class="mt-1 flex flex-wrap items-baseline gap-x-3 gap-y-1">
			<h1 class="text-2xl font-semibold tracking-tight">
				Run <span class="font-mono" title={run.id}>#{run.short}</span>
			</h1>
			<span class="flex items-center gap-1.5 text-[0.9375rem]"
				><RunBadge state={run.state} size={14} label /><span class="text-fg-muted"
					>{running
						? `for ${clock(run.durationSeconds)}`
						: `in ${clock(run.durationSeconds)}`}</span
				></span
			>
		</div>
		<RunMeta {data} class="mt-2" />
		{#if run.rerunOf}<p class="mt-2 text-xs text-fg-muted">Re-run of <a class="font-mono text-accent hover:underline" href={runHref(run.rerunOf)}>#{run.rerunOf.slice(-8).toLowerCase()}</a></p>{/if}
		<div class="mt-3"><RunActions {run} allowed={data.canManage} {onchange} /></div>
		{#if run.pendingReason && run.state === RunState.QUEUED}
			<p class="mt-3 text-[0.8125rem] text-fg-muted" data-pending={run.pendingReason}>
				{PENDING[run.pendingReason] ?? run.pendingReason}
			</p>
		{/if}
		{#if run.failure}
			<div class="mt-3 rounded-md border border-danger/40 bg-bg px-3 py-2" data-failure>
				<p class="text-[0.8125rem]">
					<span class="text-danger">Failed</span>
					{#if run.failure.check}in <span class="font-mono">{run.failure.check}</span>{/if}
					{#if run.failure.reason}<span class="text-fg-muted">· {run.failure.reason}</span>{/if}
				</p>
				{#if run.failure.lastLines.length}<pre
						class="mt-2 max-h-40 overflow-auto font-mono text-xs break-words whitespace-pre-wrap text-fg-muted">{run.failure.lastLines.join(
							'\n'
						)}</pre>{/if}
			</div>
		{/if}

		<!-- step timeline -->
		<figure class="mt-5">
			<figcaption class="sr-only">Steps, width proportional to duration</figcaption>
			<div class="flex gap-1">
				{#each run.steps as s, i (i)}
					{@const e = elapsedOf(i)}
					<a
						href={stepHref(i)}
						data-sveltekit-noscroll
						class="group min-w-0"
						style:flex-basis="{widths[i]}%"
						style:flex-grow="0"
						style:flex-shrink="1"
						aria-label="{s.name}: {RUN_LABEL[s.state]}, {clock(e)}"
					>
						<span class="relative block h-2 overflow-hidden rounded-full bg-border/70">
							{#if s.state === RunState.RUNNING}
								<span
									class="absolute inset-y-0 left-0 rounded-full bg-accent motion-safe:animate-pulse"
									style:width="{Math.min(100, (e / Math.max(e, data.usual[i] || 30)) * 100)}%"
								></span>
							{:else}
								<span class="absolute inset-0 {STATE_BG[s.state]}"></span>
							{/if}
						</span>
						<span
							class="mt-1.5 block truncate text-xs group-hover:text-fg {s.state === RunState.FAILED
								? 'text-danger'
								: 'text-fg-muted'}">{s.name}</span
						>
						<span class="tabular block truncate text-xs text-fg-subtle"
							>{s.state === RunState.QUEUED
								? `~${clock(data.usual[i])}`
								: clock(e)}{#if s.state === RunState.RUNNING && data.usual[i]}
								<span>{` / usually ${clock(data.usual[i])}`}</span>{/if}</span
						>
					</a>
				{/each}
			</div>
		</figure>

		<!-- runs on the same ref -->
		<div class="mt-5 flex flex-wrap items-center gap-x-3 gap-y-2 text-xs text-fg-subtle">
			<span
				>{run.trigger === 'worktree' ? 'Worktree runs' : 'Runs on'} <span class="font-mono text-fg-muted">{run.ref.replace('refs/heads/', '')}</span
				></span
			>
			<span class="flex flex-wrap gap-1">
				{#each older as r (r.id)}
					<a
						href={runHref(r.id)}
						class="flex min-h-5 min-w-5 items-center justify-center gap-1 rounded-sm hover:bg-surface-2 {r.id === run.id
							? 'ring-1 ring-fg'
							: ''}"
						aria-current={r.id === run.id ? 'page' : undefined}
						title="#{r.short} · {RUN_LABEL[r.state]} · {clock(r.durationSeconds)} · {relativeTime(
							toDate(r.createTime)
						)}"><RunBadge state={r.state} size={11} />{#if r.trigger === 'worktree'}<RunRef run={r} />{/if}</a
					>
				{/each}
			</span>
			{#if data.lastGreen && run.state !== RunState.SUCCEEDED}
				<span
					>last green <a
						href={runHref(data.lastGreen.id)}
						class="font-mono text-link hover:underline">#{data.lastGreen.short}</a
					>
					{relativeTime(toDate(data.lastGreen.createTime))}</span
				>
			{/if}
		</div>
	</section>
	{/if}

	<!-- what changed -->
	<BandStrip label="What changed" open={changedOpen} href={changedHref}>
		{#if data.lastGreen && run.state !== RunState.SUCCEEDED}<span
				>{data.since.length} commit{data.since.length > 1 ? 's' : ''} since
				<span class="font-mono">#{data.lastGreen.short}</span></span
			>{/if}
		<span class="tabular">{data.changes.length} files</span>
	</BandStrip>
	{#if changedOpen}
		<section class="border-b border-border px-4 py-4 sm:px-6" aria-label="What changed">
			<WhatChanged {data} />
		</section>
	{/if}

	<!-- steps, side by side -->
	<BandStrip label="Steps" open={stepsOpen} href={hrefFor({ steps: stepsOpen ? '0' : null })}>
		{#each run.steps as st, i (i)}<span class="flex items-center gap-1.5"
				><RunBadge state={st.state} size={10} />{st.name}{#if st.state !== RunState.QUEUED}<span
						class="tabular text-fg-subtle">{clock(elapsedOf(i))}</span
					>{/if}</span
			>{/each}
	</BandStrip>
	{#if stepsOpen}<StepColumns {data} />{/if}
</div>
