<!--
	One step's log. Lines are numbered per step and linkable (#L<step>-<n>);
	lines of the same flake check form a foldable group (failing groups and
	the one still running start open). Optional text filter and errors-only
	mode. Long lines wrap: no horizontal scrolling, also at 360 px.
-->
<script lang="ts">
	import { SvelteSet } from 'svelte/reactivity';
	import type { RunLogLine } from '#lib/api.ts';
	import Icon from '#lib/components/Icon.svelte';
	import { clock, segments } from '#lib/data/run.ts';

	let {
		lines,
		step,
		complete = true,
		filter = '',
		errorsOnly = false,
		allOpen = false,
		flat = false,
		offset = 0,
		label = 'Log'
	}: {
		lines: RunLogLine[];
		/** 0-based step index (anchors are 1-based). */
		step: number;
		complete?: boolean;
		filter?: string;
		errorsOnly?: boolean;
		allOpen?: boolean;
		/** No group headers: render the lines as they come. */
		flat?: boolean;
		/** Lines before these in the step (for numbering and anchors). */
		offset?: number;
		label?: string;
	} = $props();

	const segs = $derived(
		flat
			? [
					{
						group: undefined,
						lines: lines.map((line, i) => ({ n: offset + i + 1, line })),
						failed: lines.some((l) => l.stream === 'err'),
						start: lines[0]?.offsetSeconds ?? 0,
						end: lines.at(-1)?.offsetSeconds ?? 0
					}
				]
			: segments(lines, offset)
	);
	const needle = $derived(filter.trim().toLowerCase());
	const filtering = $derived(!!needle || errorsOnly);
	const keep = (l: RunLogLine) =>
		(!errorsOnly || l.stream === 'err') && (!needle || l.text.toLowerCase().includes(needle));
	const shown = $derived(
		segs
			.map((s) => ({ ...s, visible: filtering ? s.lines.filter((x) => keep(x.line)) : s.lines }))
			.filter((s) => s.visible.length)
	);

	const toggled = new SvelteSet<string>();
	const isOpen = (s: (typeof segs)[number], i: number) => {
		if (!s.group || filtering) return true;
		const byDefault = allOpen || s.failed || (!complete && i === segs.length - 1);
		return toggled.has(s.group) ? !byDefault : byDefault;
	};
	const toggle = (g: string) => (toggled.has(g) ? toggled.delete(g) : toggled.add(g));

	/** Split text around matches of the filter for highlighting. */
	function parts(text: string) {
		if (!needle) return [{ t: text, m: false }];
		const out: { t: string; m: boolean }[] = [];
		const low = text.toLowerCase();
		let i = 0;
		for (let j = low.indexOf(needle); j >= 0; j = low.indexOf(needle, i)) {
			if (j > i) out.push({ t: text.slice(i, j), m: false });
			out.push({ t: text.slice(j, j + needle.length), m: true });
			i = j + needle.length;
		}
		if (i < text.length) out.push({ t: text.slice(i), m: false });
		return out;
	}
</script>

{#snippet row(n: number, line: RunLogLine)}
	<div
		id="L{step + 1}-{n}"
		data-err={line.stream === 'err' ? '' : undefined}
		class="grid scroll-mt-20 grid-cols-[calc(6ch+1.5rem)_minmax(0,1fr)] target:bg-accent-soft hover:bg-surface-2 sm:grid-cols-[calc(6ch+1.5rem)_3rem_minmax(0,1fr)] {line.stream ===
		'err'
			? 'bg-danger-soft text-danger'
			: ''}"
	>
		<a
			href="#L{step + 1}-{n}"
			class="pr-3 text-right select-none hover:text-fg {line.stream === 'err'
				? 'text-danger'
				: 'text-fg-subtle'}"
			aria-label="Line {n}">{n}</a
		>
		<span
			class="hidden select-none sm:block {line.stream === 'err' ? 'text-danger' : 'text-fg-subtle'}"
			data-log-time
			aria-hidden="true">{clock(line.offsetSeconds)}</span
		>
		<span
			class="pr-3 break-words whitespace-pre-wrap {line.stream === 'cmd'
				? 'text-accent'
				: line.stream === 'out'
					? 'text-fg-muted'
					: ''}"
			>{#if line.stream === 'cmd'}<span class="select-none">{'$ '}</span
				>{/if}{#each parts(line.text) as p, i (i)}{#if p.m}<mark
						class="rounded-[2px] bg-warn/30 text-fg">{p.t}</mark
					>{:else}{p.t}{/if}{/each}</span
		>
	</div>
{/snippet}

<div class="font-mono text-xs leading-5" role="log" aria-label={label}>
	{#each shown as s, i (i)}
		{#if s.group}
			{@const open = isOpen(s, i)}
			<button
				type="button"
				class="flex w-full items-center gap-2 border-y border-border/50 bg-surface-2/60 py-1 pr-3 pl-3 text-left text-fg-muted hover:text-fg disabled:cursor-default"
				aria-expanded={open}
				disabled={filtering}
				onclick={() => toggle(s.group!)}
			>
				<span class="w-3 text-fg-subtle" aria-hidden="true">{open ? '▾' : '▸'}</span>
				<span class={s.failed ? 'text-danger' : 'text-fg'}>checks.{s.group}</span>
				<span class="text-fg-subtle">{s.lines.length} lines</span>
				<span class="ml-auto flex items-center gap-2 text-fg-subtle">
					{#if s.failed}<span class="text-danger">failed</span
						>{:else if !complete && i === segs.length - 1}<span class="text-accent">running</span
						>{:else}<Icon name="check" size={12} class="text-ok" />{/if}
					<span class="tabular">{clock(Math.max(0, s.end - s.start))}</span>
				</span>
			</button>
			{#if open}
				{#each s.visible as x (x.n)}{@render row(x.n, x.line)}{/each}
			{/if}
		{:else}
			{#each s.visible as x (x.n)}{@render row(x.n, x.line)}{/each}
		{/if}
	{:else}
		<p class="px-3 py-2 font-sans text-fg-subtle">
			{filtering ? 'No line matches.' : complete ? 'No output.' : 'Waiting for output…'}
		</p>
	{/each}
	{#if !complete && !filtering}
		<p class="flex items-center gap-2 px-3 py-1.5 text-accent">
			<span class="inline-block h-3 w-1.5 bg-accent motion-safe:animate-pulse" aria-hidden="true"
			></span><span class="font-sans">Streaming…</span>
		</p>
	{/if}
</div>
