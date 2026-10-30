<!--
	Activity: git activity across every visible repository (default branches).
	Period in the URL (`?days=7|30|90`) so it can be bookmarked.
-->
<script lang="ts">
	import { page } from '$app/state';
	import { getActivity } from '#lib/api.ts';
	import DailyBars from '#lib/components/DailyBars.svelte';
	import Skeleton from '#lib/components/Skeleton.svelte';
	import StateView from '#lib/components/StateView.svelte';
	import { relativeTime, toDate } from '#lib/format.ts';
	import { commitHref } from '#lib/repo-ui.ts';
	import { resource } from '#lib/resource.svelte.ts';

	const PERIODS = [7, 30, 90] as const;
	const days = $derived(PERIODS.find((d) => String(d) === page.url.searchParams.get('days')) ?? 30);
	const activity = resource(() => getActivity({ days }));

	const a = $derived(activity.data);
	const total = $derived(a ? a.perDay.reduce((s, v) => s + v, 0) : 0);
	const activeDays = $derived(a ? a.perDay.filter((v) => v > 0).length : 0);
	const busiest = $derived.by(() => {
		if (!a) return undefined;
		const i = a.perDay.indexOf(Math.max(...a.perDay));
		return {
			value: a.perDay[i],
			date: new Date(Date.now() - (a.perDay.length - 1 - i) * 86400000)
		};
	});
	const repoMax = $derived(Math.max(1, ...(a?.perRepo.map((r) => r.commits) ?? [])));
	const fmtDay = new Intl.DateTimeFormat('en', {
		weekday: 'short',
		month: 'short',
		day: 'numeric'
	});

	const toRepo = (key: string) => {
		const [owner, name] = key.split('/');
		return { owner, name };
	};
</script>

<svelte:head><title>Activity · kajiba</title></svelte:head>

<main class="mx-auto max-w-6xl px-4 pt-5 pb-16 sm:px-6 lg:px-8 lg:pt-7">
	<header class="flex flex-wrap items-end gap-3">
		<div class="flex-1">
			<h1 class="text-xl font-semibold tracking-tight">Activity</h1>
			<p class="text-[0.8125rem] text-fg-muted">
				Commits on default branches, across repositories visible to you.
			</p>
		</div>
		<nav
			class="flex rounded-sm border border-border bg-surface p-0.5 text-[0.8125rem]"
			aria-label="Period"
		>
			{#each PERIODS as d (d)}
				<a
					href="?days={d}"
					aria-current={d === days ? 'true' : undefined}
					class="rounded-[3px] px-3 py-1 {d === days
						? 'bg-accent-soft font-medium text-accent'
						: 'text-fg-muted hover:text-fg'}">{d} days</a
				>
			{/each}
		</nav>
	</header>

	{#if activity.error}
		<StateView error={activity.error} onretry={activity.retry} />
	{:else if !a}
		<div class="mt-6 rounded-md border border-border bg-surface p-5"><Skeleton lines={10} /></div>
	{:else}
		<div class="transition-opacity {activity.loading ? 'opacity-60' : ''}">
			<!-- Headline figures -->
			<dl
				class="mt-6 grid grid-cols-2 gap-px overflow-hidden rounded-md border border-border bg-border md:grid-cols-4"
			>
				{#each [['Commits', String(total), `${days} days`], ['Active repositories', String(a.perRepo.length), 'with at least one commit'], ['Active days', `${activeDays}/${days}`, 'days with commits'], ['Busiest day', String(busiest?.value ?? 0), busiest ? fmtDay.format(busiest.date) : '']] as [k, v, note] (k)}
					<div class="bg-surface px-4 py-3">
						<dt class="text-xs text-fg-subtle">{k}</dt>
						<dd class="tabular mt-0.5 text-2xl font-semibold tracking-tight">{v}</dd>
						<dd class="text-xs text-fg-subtle">{note}</dd>
					</div>
				{/each}
			</dl>

			<!-- Commits per day -->
			<section class="mt-6 rounded-md border border-border bg-surface p-4">
				<h2 class="mb-3 text-[0.8125rem] font-medium">Commits per day</h2>
				{#if total === 0}
					<p class="py-10 text-center text-[0.8125rem] text-fg-subtle">
						No commits in this period.
					</p>
				{:else}
					<DailyBars values={a.perDay} label="Commits per day over the last {days} days" />
				{/if}
			</section>

			<div class="mt-6 grid gap-6 lg:grid-cols-[minmax(0,2fr)_minmax(0,3fr)]">
				<div class="space-y-6">
					<!-- By repository -->
					<section class="rounded-md border border-border bg-surface p-4">
						<h2 class="mb-3 text-[0.8125rem] font-medium">By repository</h2>
						<ul class="space-y-2.5">
							{#each a.perRepo as r (r.repo)}
								<li>
									<div class="flex justify-between text-[0.8125rem]">
										<a href="/{r.repo}" class="truncate hover:text-link hover:underline">{r.repo}</a
										>
										<span class="tabular text-fg-muted">{r.commits}</span>
									</div>
									<div class="mt-1 h-1.5 rounded-full bg-surface-2" aria-hidden="true">
										<div
											class="h-full rounded-full bg-accent"
											style:width="{(r.commits / repoMax) * 100}%"
										></div>
									</div>
								</li>
							{:else}
								<li class="text-[0.8125rem] text-fg-subtle">No activity.</li>
							{/each}
						</ul>
					</section>

					<!-- By author -->
					<section class="rounded-md border border-border bg-surface p-4">
						<h2 class="mb-3 text-[0.8125rem] font-medium">By author</h2>
						<ul class="space-y-1 text-[0.8125rem]">
							{#each a.perAuthor as p (p.authorName)}
								<li class="flex justify-between">
									<span>{p.authorName}</span>
									<span class="tabular text-fg-muted"
										>{p.commits}
										<span class="text-fg-subtle">· {Math.round((p.commits / total) * 100)}%</span
										></span
									>
								</li>
							{/each}
						</ul>
					</section>
				</div>

				<!-- Recent commits -->
				<section class="min-w-0 rounded-md border border-border bg-surface">
					<h2 class="border-b border-border px-4 py-3 text-[0.8125rem] font-medium">
						Latest commits
					</h2>
					<ul class="divide-y divide-border">
						{#each a.recent as c (c.id)}
							{@const when = toDate(c.commitTime)}
							<li class="px-4 py-2.5">
								<a
									href={commitHref(toRepo(c.repo), c.id)}
									class="line-clamp-1 text-[0.8125rem] hover:text-link hover:underline"
									>{c.subject}</a
								>
								<p class="text-xs text-fg-subtle">
									<a
										href="/{c.repo}"
										class="text-fg-muted underline decoration-border-strong underline-offset-2 hover:decoration-current"
										>{c.repo}</a
									>
									· {c.authorName} ·
									<time datetime={when?.toISOString()}>{relativeTime(when)}</time>
								</p>
							</li>
						{:else}
							<li class="px-4 py-6 text-center text-[0.8125rem] text-fg-subtle">No commits.</li>
						{/each}
					</ul>
				</section>
			</div>
		</div>
	{/if}
</main>
