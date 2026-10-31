<!--
	A run's steps side by side, like the repository's file columns: the step
	in focus is open and shows its console; the others fold into vertical
	spines (finished ones on the left, queued ones on the right). Inside the
	open step, its flake checks are columns too (fmt, clippy, … tests), the
	checks not reached yet folded on the right. Each step spine carries a
	gauge: its duration against the usual one (median of recent green runs).
	Below `sm` the spines become rows and everything stacks.
	URL: ?step=<n> (1-based), ?check=<name>.
-->
<script lang="ts">
	import { page } from '$app/state';
	import { RunState } from '#lib/api.ts';
	import RunBadge, { RUN_LABEL } from '#lib/components/RunBadge.svelte';
	import LogView from '#lib/components/LogView.svelte';
	import { clock, focusStep, segments, type RunPage } from '#lib/data/run.ts';

	let { data }: { data: RunPage } = $props();
	const run = $derived(data.run);

	const q = $derived(page.url.searchParams);
	function hrefFor(next: Record<string, string | null>) {
		const u = new URLSearchParams(q.toString());
		for (const [k, v] of Object.entries(next)) v === null ? u.delete(k) : u.set(k, v);
		return `?${u}`;
	}
	const step = $derived.by(() => {
		const n = Number(q.get('step'));
		return n >= 1 && n <= run.steps.length ? n - 1 : focusStep(run);
	});
	const stepHref = (i: number) => hrefFor({ step: String(i + 1), check: null });

	// ── durations against usual ───────────────────────────────────────
	const elapsedOf = (i: number) => {
		const s = run.steps[i];
		if (s.state !== RunState.RUNNING) return s.durationSeconds;
		const before = run.steps.slice(0, i).reduce((a, x) => a + x.durationSeconds, 0);
		return Math.max(0, run.durationSeconds - before);
	};
	const scale = $derived(
		Math.max(1, ...run.steps.map((_, i) => Math.max(elapsedOf(i), data.usual[i] ?? 0)))
	);
	const pct = (s: number) => `${Math.min(100, (s / scale) * 100)}%`;
	const signed = (s: number) => `${s < 0 ? '−' : '+'}${clock(Math.abs(s))}`;
	const FILL: Record<RunState, string> = {
		[RunState.UNSPECIFIED]: 'bg-border',
		[RunState.QUEUED]: 'bg-transparent',
		[RunState.RUNNING]: 'bg-accent motion-safe:animate-pulse',
		[RunState.SUCCEEDED]: 'bg-ok/70',
		[RunState.FAILED]: 'bg-danger',
		[RunState.CANCELLED]: 'bg-fg-subtle/50'
	};

	// ── the open step: lines before, checks, lines after ───────────────
	const s = $derived(run.steps[step]);
	const log = $derived(data.logs[step]);
	const segs = $derived(segments(log?.lines ?? []));
	const firstG = $derived(segs.findIndex((x) => x.group));
	const lastG = $derived(segs.findLastIndex((x) => x.group));
	const pre = $derived(firstG < 0 ? [] : segs.slice(0, firstG).flatMap((x) => x.lines));
	const post = $derived(lastG < 0 ? [] : segs.slice(lastG + 1).flatMap((x) => x.lines));
	type Check = { name: string; seg?: (typeof segs)[number]; state: RunState };
	const checks = $derived.by<Check[]>(() => {
		const seen = segs.filter((x) => x.group);
		const out: Check[] = seen.map((seg, k) => ({
			name: seg.group!,
			seg,
			state: seg.failed
				? RunState.FAILED
				: !log.complete && k === seen.length - 1
					? RunState.RUNNING
					: RunState.SUCCEEDED
		}));
		for (const name of s.checks ?? [])
			if (!out.some((c) => c.name === name))
				out.push({
					name,
					state: s.state === RunState.RUNNING ? RunState.QUEUED : RunState.CANCELLED
				});
		return out;
	});
	const check = $derived.by(() => {
		const want = q.get('check');
		if (want && checks.some((c) => c.name === want && c.seg)) return want;
		return (
			checks.find((c) => c.state === RunState.FAILED) ??
			checks.find((c) => c.state === RunState.RUNNING) ??
			checks.findLast((c) => c.seg)
		)?.name;
	});
	const errors = $derived(log?.lines.filter((l) => l.stream === 'err').length ?? 0);

	// Land on what matters: a linked line, else the first error, else follow the
	// end of a running log (unless the reader scrolled up).
	let pane = $state<HTMLElement>();
	let stick = true;
	const onscroll = (e: Event) => {
		const el = e.currentTarget as HTMLElement;
		stick = el.scrollHeight - el.scrollTop - el.clientHeight < 48;
	};
	$effect(() => {
		void step;
		void check;
		void log?.lines.length;
		if (!pane) return;
		const to = (el: HTMLElement) =>
			(pane!.scrollTop = Math.max(0, el.offsetTop - pane!.offsetTop - 48));
		const linked = page.url.hash && document.getElementById(page.url.hash.slice(1));
		const err = pane.querySelector<HTMLElement>('[data-err]');
		if (log?.complete) {
			if (linked && pane.contains(linked)) to(linked);
			else if (err) to(err);
		} else if (stick) pane.scrollTop = pane.scrollHeight;
	});
</script>

{#snippet gauge(i: number)}
	{@const e = elapsedOf(i)}
	{@const u = data.usual[i] ?? 0}
	<span
		class="relative block h-1.5 w-14 shrink-0 rounded-full bg-border/60 sm:h-14 sm:w-1.5"
		style:--p={pct(e)}
		style:--u={pct(u)}
		aria-hidden="true"
	>
		{#if run.steps[i].state !== RunState.QUEUED}<span
				class="absolute rounded-full max-sm:inset-y-0 max-sm:left-0 max-sm:w-(--p) sm:inset-x-0 sm:bottom-0 sm:h-(--p) {FILL[
					run.steps[i].state
				]}"
			></span>{/if}
		{#if u}<span
				class="absolute bg-fg max-sm:-inset-y-0.5 max-sm:left-(--u) max-sm:w-px sm:-inset-x-0.5 sm:bottom-(--u) sm:h-px"
			></span>{/if}
	</span>
{/snippet}

{#snippet stepSpine(i: number)}
	{@const st = run.steps[i]}
	<a
		href={stepHref(i)}
		data-sveltekit-noscroll
		aria-label="{st.name}: {RUN_LABEL[st.state]}{st.state === RunState.QUEUED
			? ''
			: `, ${clock(elapsedOf(i))}`}{data.usual[i] ? `, usually ${clock(data.usual[i])}` : ''}"
		class="flex shrink-0 items-center gap-3 border-b border-border px-4 py-2 text-xs hover:bg-surface sm:w-10 sm:flex-col sm:gap-2.5 sm:border-r sm:border-b-0 sm:px-0 sm:py-3 {st.state ===
		RunState.QUEUED
			? 'text-fg-subtle'
			: 'text-fg-muted hover:text-fg'}"
	>
		<RunBadge state={st.state} size={12} />
		<span class="tracking-wide whitespace-nowrap uppercase sm:[writing-mode:vertical-rl]"
			>{st.name}</span
		>
		<span class="flex-1"></span>
		{@render gauge(i)}
		<span class="tabular text-[0.6875rem] text-fg-subtle"
			>{st.state === RunState.QUEUED ? '—' : clock(elapsedOf(i))}</span
		>
	</a>
{/snippet}

{#snippet checkSpine(c: Check)}
	{#if c.seg}
		<a
			href={hrefFor({ step: String(step + 1), check: c.name })}
			data-sveltekit-noscroll
			aria-label="checks.{c.name}: {RUN_LABEL[c.state]}"
			class="flex shrink-0 items-center gap-3 border-b border-border/60 px-4 py-1.5 font-mono text-xs text-fg-muted hover:bg-surface-2 hover:text-fg sm:w-8 sm:flex-col sm:gap-2 sm:border-r sm:border-b-0 sm:px-0 sm:py-2.5"
			><RunBadge state={c.state} size={11} /><span class="sm:[writing-mode:vertical-rl]"
				>{c.name}</span
			><span class="flex-1"></span><span class="tabular text-[0.6875rem] text-fg-subtle"
				>{clock(Math.max(0, c.seg.end - c.seg.start))}</span
			></a
		>
	{:else}
		<div
			class="flex shrink-0 items-center gap-3 border-b border-border/60 px-4 py-1.5 font-mono text-xs text-fg-subtle sm:w-8 sm:flex-col sm:gap-2 sm:border-r sm:border-b-0 sm:px-0 sm:py-2.5"
			aria-label="checks.{c.name}: {c.state === RunState.QUEUED ? 'queued' : 'not run'}"
			role="img"
		>
			<RunBadge state={c.state} size={11} /><span class="sm:[writing-mode:vertical-rl]"
				>{c.name}</span
			>
		</div>
	{/if}
{/snippet}

<div
	class="flex flex-col border-b border-border sm:h-[min(72vh,46rem)] sm:min-h-[26rem] sm:flex-row"
	id="steps"
>
	{#each run.steps as st, i (st.name)}
		{#if i !== step}
			{@render stepSpine(i)}
		{:else}
			<section
				class="flex min-h-0 min-w-0 flex-1 flex-col border-b border-border bg-bg sm:border-r sm:border-b-0"
				aria-label="{st.name} log"
			>
				<!-- step header: state, duration against usual -->
				<header
					class="flex flex-wrap items-center gap-x-3 gap-y-1 border-b border-border bg-surface px-4 py-2 text-xs"
				>
					<h2 class="font-medium tracking-wide text-fg uppercase">{st.name}</h2>
					<RunBadge state={st.state} size={12} label />
					{#if st.state !== RunState.QUEUED}
						{@const e = elapsedOf(i)}
						{@const u = data.usual[i] ?? 0}
						<span
							class="relative block h-1.5 w-28 rounded-full bg-border/60"
							style:--p={pct(e)}
							style:--u={pct(u)}
							aria-hidden="true"
							><span class="absolute inset-y-0 left-0 w-(--p) rounded-full {FILL[st.state]}"
							></span>{#if u}<span class="absolute -inset-y-0.5 left-(--u) w-px bg-fg"
								></span>{/if}</span
						>
						<span class="tabular text-fg-muted"
							>{clock(e)}{#if u}<span class="text-fg-subtle">{` · usually ${clock(u)}`}</span
								>{#if st.state !== RunState.RUNNING}<span
										class="ml-1.5 {e - u > 15 ? 'text-warn' : 'text-fg-subtle'}"
										>{signed(e - u)}</span
									>{/if}{/if}</span
						>
					{/if}
					<span class="tabular ml-auto text-fg-subtle"
						>{log?.lines.length ?? 0} lines{#if errors}<span class="text-danger"
								>{` · ${errors} errors`}</span
							>{/if}</span
					>
				</header>

				{#if st.state === RunState.QUEUED}
					<p class="px-4 py-3 text-xs text-fg-subtle">
						Not started yet{data.usual[i] ? ` — usually takes ${clock(data.usual[i])}` : ''}.
					</p>
				{:else if checks.length}
					{#if pre.length}
						<div class="border-b border-border/60 py-1">
							<LogView flat lines={pre.map((x) => x.line)} step={i} label="{st.name} log" />
						</div>
					{/if}
					<div class="flex min-h-0 flex-1 flex-col sm:flex-row">
						{#each checks as c (c.name)}
							{#if c.name === check && c.seg}
								<section
									class="flex min-h-0 min-w-0 flex-1 flex-col border-b border-border/60 sm:border-r sm:border-b-0"
									aria-label="checks.{c.name}"
								>
									<h3
										class="flex items-center gap-2 border-b border-border/60 bg-surface-2/50 px-3 py-1 font-mono text-xs"
									>
										<RunBadge state={c.state} size={11} />
										<span class={c.state === RunState.FAILED ? 'text-danger' : 'text-fg'}
											>checks.{c.name}</span
										>
										<span class="tabular ml-auto text-fg-subtle"
											>{c.seg.lines.length} lines · {clock(
												Math.max(0, c.seg.end - c.seg.start)
											)}</span
										>
									</h3>
									<!-- svelte-ignore a11y_no_noninteractive_tabindex (scrollable: must be focusable) -->
									<div
										bind:this={pane}
						{onscroll}
										tabindex="0"
										class="relative min-h-0 flex-1 overflow-y-auto py-1 max-sm:max-h-[60vh]"
									>
										<LogView
											flat
											lines={c.seg.lines.map((x) => x.line)}
											offset={c.seg.lines[0].n - 1}
											step={i}
											complete={c.state !== RunState.RUNNING}
											label="checks.{c.name} log"
										/>
									</div>
								</section>
							{:else}
								{@render checkSpine(c)}
							{/if}
						{/each}
					</div>
					{#if post.length}
						<!-- svelte-ignore a11y_no_noninteractive_tabindex (scrollable: must be focusable) -->
						<div
							tabindex="0"
							class="relative max-h-40 shrink-0 overflow-y-auto border-t border-border py-1"
						>
							<LogView
								flat
								lines={post.map((x) => x.line)}
								offset={post[0].n - 1}
								step={i}
								label="{st.name} summary"
							/>
						</div>
					{/if}
				{:else}
					<!-- svelte-ignore a11y_no_noninteractive_tabindex (scrollable: must be focusable) -->
					<div
						bind:this={pane}
						{onscroll}
						tabindex="0"
						class="relative min-h-0 flex-1 overflow-y-auto py-1 max-sm:max-h-[60vh]"
					>
						<LogView
							flat
							lines={log.lines}
							step={i}
							complete={log.complete}
							label="{st.name} log"
						/>
					</div>
				{/if}
			</section>
		{/if}
	{/each}
</div>
