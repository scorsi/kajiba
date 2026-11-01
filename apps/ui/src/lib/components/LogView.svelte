<!-- Only the viewport and a small overscan own DOM nodes; all lines remain searchable. -->
<script lang="ts">
	import { onMount, tick } from 'svelte';
	import type { RunLogLine } from '#lib/api.ts';
	import { clock } from '#lib/data/run.ts';
	import { lineWindow, linePositions, lineTop, scrollScale } from '#lib/log-window.ts';
	let {
		lines,
		step,
		complete = true,
		filter = '',
		errorsOnly = false,
		offset = 0,
		label = 'Log'
	}: {
		lines: RunLogLine[];
		step: number;
		complete?: boolean;
		filter?: string;
		errorsOnly?: boolean;
		offset?: number;
		label?: string;
	} = $props();
	let root: HTMLElement;
	let pane: HTMLElement;
	let top = $state(0);
	let viewport = $state(600);
	let revision = $state(0);
	const heights = new Map<number, number>();
	const needle = $derived(filter.trim().toLowerCase());
	const filtering = $derived(!!needle || errorsOnly);
	const indices = $derived.by(() => {
		const out = new Uint32Array(lines.length);
		let count = 0;
		for (let i = 0; i < lines.length; i++) {
			const l = lines[i];
			if ((!errorsOnly || l.stream === 'err') && (!needle || l.text.toLowerCase().includes(needle)))
				out[count++] = i;
		}
		return out.subarray(0, count);
	});
	const positions = $derived.by(() => {
		void revision;
		return linePositions(indices, heights);
	});
	const window = $derived(lineWindow(positions, top, viewport));
	function origin() {
		return root.getBoundingClientRect().top - pane.getBoundingClientRect().top + pane.scrollTop;
	}
	function update() {
		if (!pane || !root) return;
		top = Math.max(0, pane.scrollTop - origin());
		viewport = pane.clientHeight;
	}
	function measure(node: HTMLElement, index: number) {
		const observer = new ResizeObserver(() => {
			const height = node.getBoundingClientRect().height;
			if (height > 0 && (heights.get(index) ?? 20) !== height) {
				const bottom = top + viewport >= Math.min(8_000_000, positions[positions.length - 1]) - 4;
				const above = node.getBoundingClientRect().bottom <= pane.getBoundingClientRect().top;
				const delta = height - (heights.get(index) ?? 20);
				heights.set(index, height);
				revision++;
				if (bottom || above)
					void tick().then(() => {
						if (bottom) pane.scrollTop = pane.scrollHeight;
						else pane.scrollTop += delta / scrollScale(positions, viewport);
						update();
					});
			}
		});
		observer.observe(node);
		return { destroy: () => observer.disconnect() };
	}
	/** Resolve against the full list, before the target has a DOM node. */
	export async function scrollToLine(n: number) {
		const index = indices.indexOf(n - offset - 1);
		if (!pane || index < 0) return false;
		pane.scrollTop = origin() + lineTop(positions, index, viewport);
		update();
		await tick();
		const target = document.getElementById(`L${step + 1}-${n}`);
		if (target && root.contains(target)) {
			pane.scrollTop +=
				(target.getBoundingClientRect().top - pane.getBoundingClientRect().top - 4) /
				scrollScale(positions, viewport);
			update();
		}
		return true;
	}
	onMount(() => {
		pane = root.closest<HTMLElement>('[data-log-pane]') ?? root.parentElement!;
		pane.addEventListener('scroll', update, { passive: true });
		const resize = new ResizeObserver(update);
		resize.observe(pane);
		update();
		return () => {
			pane.removeEventListener('scroll', update);
			resize.disconnect();
		};
	});
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
		use:measure={n - offset - 1}
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

<div bind:this={root} class="relative font-mono text-xs leading-5" role="log" aria-label={label}>
	<div style:height="{window.before}px" aria-hidden="true"></div>
	{#each indices.slice(window.start, window.end) as index (index)}
		{@render row(offset + index + 1, lines[index])}
	{:else}
		{#if !indices.length}
			<p class="px-3 py-2 font-sans text-fg-subtle">
				{filtering ? 'No line matches.' : complete ? 'No output.' : 'Waiting for output…'}
			</p>
		{/if}
	{/each}
	<div style:height="{window.after}px" aria-hidden="true"></div>
	{#if !complete && !filtering}
		<p class="flex items-center gap-2 px-3 py-1.5 text-accent">
			<span class="inline-block h-3 w-1.5 bg-accent motion-safe:animate-pulse" aria-hidden="true"></span>
			<span class="font-sans">Streaming…</span>
		</p>
	{/if}
</div>
