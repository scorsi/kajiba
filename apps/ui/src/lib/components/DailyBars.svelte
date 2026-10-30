<!--
	Single-series column chart (one value per day). Single hue, no legend (the
	title names the series), hairline grid, 4px rounded data-ends, 2px gaps,
	per-bar hover tooltip with a hit target the full band height, table
	alternative for screen readers and keyboard users.
-->
<script lang="ts">
	let {
		values,
		endDate = new Date(),
		label,
		unit = 'commits'
	}: { values: number[]; endDate?: Date; label: string; unit?: string } = $props();

	const H = 168;
	const PAD = { top: 8, right: 4, bottom: 22, left: 28 };
	let width = $state(600);
	let hover = $state<number | null>(null);

	const max = $derived(Math.max(1, ...values));
	const step = $derived(niceStep(max));
	const top = $derived(Math.ceil(max / step) * step);
	const ticks = $derived(Array.from({ length: Math.floor(top / step) + 1 }, (_, i) => i * step));
	const plotW = $derived(Math.max(10, width - PAD.left - PAD.right));
	const plotH = H - PAD.top - PAD.bottom;
	const band = $derived(plotW / values.length);
	const barW = $derived(Math.max(1, Math.min(24, band - 2)));
	const y = (v: number) => PAD.top + plotH - (v / top) * plotH;

	const dates = $derived(
		values.map((_, i) => new Date(endDate.getTime() - (values.length - 1 - i) * 86400000))
	);
	const fmt = new Intl.DateTimeFormat('en', { month: 'short', day: 'numeric' });
	const fmtLong = new Intl.DateTimeFormat('en', {
		weekday: 'short',
		month: 'short',
		day: 'numeric'
	});
	const xLabels = $derived([0, Math.floor((values.length - 1) / 2), values.length - 1]);

	function niceStep(m: number): number {
		const raw = m / 4;
		const pow = 10 ** Math.floor(Math.log10(raw || 1));
		return [1, 2, 5, 10].map((k) => k * pow).find((s) => s >= raw) ?? pow * 10;
	}

	/** Column with rounded top corners only (square at the baseline). */
	function barPath(x: number, v: number): string {
		if (v <= 0) return '';
		const h = Math.max(2, (v / top) * plotH);
		const r = Math.min(4, barW / 2, h);
		const y0 = PAD.top + plotH;
		const y1 = y0 - h;
		return `M${x},${y0}V${y1 + r}Q${x},${y1} ${x + r},${y1}H${x + barW - r}Q${x + barW},${y1} ${x + barW},${y1 + r}V${y0}Z`;
	}
</script>

<figure class="relative">
	<figcaption class="sr-only">{label}</figcaption>
	<div bind:clientWidth={width} class="relative">
		<svg {width} height={H} role="img" aria-label={label} class="block overflow-visible">
			{#each ticks as t (t)}
				<line
					x1={PAD.left}
					x2={width - PAD.right}
					y1={y(t)}
					y2={y(t)}
					class="stroke-border"
					stroke-width="1"
				/>
				<text
					x={PAD.left - 6}
					y={y(t)}
					text-anchor="end"
					dominant-baseline="middle"
					class="tabular fill-fg-subtle text-[10px]">{t}</text
				>
			{/each}
			{#each values as v, i (i)}
				{@const x = PAD.left + i * band + (band - barW) / 2}
				<path
					d={barPath(x, v)}
					class={hover === null || hover === i ? 'fill-accent' : 'fill-accent/45'}
				/>
				<!-- hit target: the whole band, taller than the mark -->
				<rect
					x={PAD.left + i * band}
					y={PAD.top}
					width={band}
					height={plotH}
					fill="transparent"
					role="presentation"
					onpointerenter={() => (hover = i)}
					onpointerleave={() => (hover = null)}
				/>
			{/each}
			{#each xLabels as i (i)}
				<text
					x={PAD.left + i * band + band / 2}
					y={H - 6}
					text-anchor={i === 0 ? 'start' : i === values.length - 1 ? 'end' : 'middle'}
					class="fill-fg-subtle text-[10px]">{fmt.format(dates[i])}</text
				>
			{/each}
		</svg>

		{#if hover !== null}
			<div
				class="pointer-events-none absolute z-10 -translate-x-1/2 rounded-sm border border-border bg-surface px-2 py-1 text-xs whitespace-nowrap shadow-md"
				style:left="{Math.min(Math.max(PAD.left + hover * band + band / 2, 60), width - 60)}px"
				style:top="{Math.max(0, y(values[hover]) - 40)}px"
			>
				<div class="text-fg-subtle">{fmtLong.format(dates[hover])}</div>
				<div class="tabular font-medium text-fg">{values[hover]} {unit}</div>
			</div>
		{/if}
	</div>

	<details class="mt-2 text-xs">
		<summary class="cursor-pointer text-fg-subtle hover:text-fg">Show as table</summary>
		<table class="tabular mt-2 w-full max-w-xs text-left">
			<thead
				><tr class="text-fg-subtle"
					><th class="font-normal">Day</th><th class="text-right font-normal">{unit}</th></tr
				></thead
			>
			<tbody>
				{#each values as v, i (i)}
					<tr class="border-t border-border"
						><td class="py-0.5">{fmtLong.format(dates[i])}</td><td class="text-right">{v}</td></tr
					>
				{/each}
			</tbody>
		</table>
	</details>
</figure>
