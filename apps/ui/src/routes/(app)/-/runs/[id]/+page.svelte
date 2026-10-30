<!--
	/-/runs/<id>: CI run — PLACEHOLDER (later milestone). Shows what is known
	from the runners (repo, ref, runner) and the shape of the future page.
-->
<script lang="ts">
	import { page } from '$app/state';
	import { listRunners } from '#lib/api.ts';
	import Icon from '#lib/components/Icon.svelte';
	import { refShortName, relativeTime, toDate } from '#lib/format.ts';
	import { resource } from '#lib/resource.svelte.ts';

	const id = $derived(page.params.id);
	const runner = resource(() =>
		listRunners().then((r) => r.runners.find((x) => x.currentRun?.id === page.params.id))
	);
	const run = $derived(runner.data?.currentRun);
	const STEPS = ['Fetch', 'Evaluate flake', 'nix flake check', 'Report'];
</script>

<svelte:head><title>Run #{id} · kajiba</title></svelte:head>

<main class="mx-auto max-w-5xl px-4 pt-5 pb-16 sm:px-6 lg:px-8 lg:pt-7">
	<p
		class="mb-5 flex items-center gap-2 rounded-md bg-surface-2 px-3 py-2 text-[0.8125rem] text-fg-muted"
	>
		<Icon name="play" size={14} /> CI runs come in a later milestone. This page is a placeholder.
	</p>

	<header class="flex flex-wrap items-baseline gap-x-3 gap-y-1">
		<h1 class="text-xl font-semibold tracking-tight">Run <span class="font-mono">#{id}</span></h1>
		{#if run}
			<span
				class="inline-flex items-center gap-1.5 rounded-full bg-accent-soft px-2 py-0.5 text-xs text-accent"
			>
				<span class="size-1.5 rounded-full bg-accent motion-safe:animate-pulse"></span> running
			</span>
			<span class="text-[0.8125rem] text-fg-muted">
				<a href="/{run.repo}" class="hover:underline">{run.repo}</a> ·
				<span class="font-mono">{refShortName(run.ref)}</span> · on
				<span class="font-mono">{runner.data?.name}</span> · started {relativeTime(
					toDate(run.startTime)
				)}
			</span>
		{:else if runner.data === undefined && !runner.loading}
			<span class="text-[0.8125rem] text-fg-subtle">No details available yet.</span>
		{/if}
	</header>

	<div class="mt-6 grid gap-5 lg:grid-cols-[14rem_minmax(0,1fr)]" aria-hidden="true">
		<ol class="space-y-1 rounded-md border border-border bg-surface p-2 text-[0.8125rem]">
			{#each STEPS as s, i (s)}
				<li
					class="flex items-center gap-2 rounded-sm px-2 py-1.5 {i === 2
						? 'bg-surface-2 text-fg'
						: 'text-fg-subtle'}"
				>
					<span
						class="size-2 rounded-full {i < 2
							? 'bg-ok'
							: i === 2
								? 'bg-accent'
								: 'border border-fg-subtle'}"
					></span>
					{s}
				</li>
			{/each}
		</ol>
		<div class="min-h-72 rounded-md border border-border bg-surface-2 p-4 font-mono text-xs">
			<div class="space-y-2 opacity-60">
				{#each [70, 45, 88, 52, 64, 30, 76] as w, i (i)}
					<div class="h-2.5 rounded-sm bg-border" style:width="{w}%"></div>
				{/each}
			</div>
			<p class="mt-6 text-fg-subtle">Live logs will stream here.</p>
		</div>
	</div>
</main>
