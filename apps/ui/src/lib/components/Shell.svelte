<!--
	The app shell: the whole forge as one foldable band on top of every page.
	Folded (default): a 36 px strip — ▸ kajiba, where you are, user, theme.
	Unfolded (`?drawer=1`): it takes the whole screen instead of the page —
	repositories (30-day activity, last change, last CI state) and pages.
	Picking anything folds it again (the link drops `drawer`).
	Where you are is derived from the route; `location` overrides it.
-->
<script lang="ts">
	import { uniqueCommits } from '#lib/data/forge.ts';
	import { goto } from '$app/navigation';
	import { page } from '$app/state';
	import type { Snippet } from 'svelte';
	import { Code, getViewer, logout, listCommits, type Run } from '#lib/api.ts';
	import Icon from '#lib/components/Icon.svelte';
	import RunBadge from '#lib/components/RunBadge.svelte';
	import StateView from '#lib/components/StateView.svelte';
	import ThemeToggle from '#lib/components/ThemeToggle.svelte';
	import { relativeTime, toDate } from '#lib/format.ts';
	import { allRepos, recentRuns } from '#lib/loaders.ts';
	import { resource } from '#lib/resource.svelte.ts';
	import { isCurrentPage } from '#lib/shell-navigation.ts';

	let { location, children }: { location?: string; children: Snippet } = $props();

	const viewer = resource(() => getViewer());
	// A signed-out visitor sees the sign-in state, not pages that cannot load.
	const isUnauthenticated = $derived(
		typeof viewer.error === 'object' &&
			viewer.error !== null &&
			'code' in viewer.error &&
			viewer.error.code === Code.Unauthenticated
	);

	async function signOut() {
		await logout();
		await goto('/-/login');
	}

	/** Where you are, from the route. */
	const here = $derived.by(() => {
		if (location) return { text: location, mono: false };
		const p = page.params;
		switch (page.route.id) {
			case '/(app)':
				return { text: 'activity', mono: false };
			case '/(app)/[owner=slug]/[name=slug]':
				return { text: `${p.owner}/${p.name}`, mono: true };
			case '/(app)/-/runs/[id]':
				return { text: `run #${p.id}`, mono: false };
			case '/(app)/-/admin':
				return { text: 'administration', mono: false };
			case '/(app)/-/account':
				return { text: 'account', mono: false };
			default:
				return { text: '', mono: false };
		}
	});
	const repoKey = $derived(
		page.params.owner && page.params.name
			? `${page.params.owner}/${page.params.name}`
			: ''
	);

	const ALL_PAGES = [
		{ label: 'Activity', href: '/', icon: 'chart' as const, admin: false },
		{ label: 'Administration', href: '/-/admin', icon: 'server' as const, admin: true },
		{ label: 'Account', href: '/-/account', icon: 'user' as const, admin: false }
	];
	// Administration is only listed for administrators.
	const PAGES = $derived(ALL_PAGES.filter((p) => !p.admin || viewer.data?.admin));

	// ── the unfolded overview ──
	const drawerOpen = $derived(page.url.searchParams.get('drawer') === '1');
	function drawerHref(open: boolean) {
		const u = new URLSearchParams(page.url.searchParams.toString());
		if (open) u.set('drawer', '1');
		else u.delete('drawer');
		const s = u.toString();
		return s ? `?${s}` : page.url.pathname;
	}
	const overview = resource(async () => {
		if (!drawerOpen) return undefined;
		const list = await allRepos();
		const [runs, logs] = await Promise.all([
			recentRuns(3),
			Promise.all(
				list.map((r) =>
					listCommits({ repo: `${r.owner}/${r.name}`, revision: r.defaultBranch, want: 120 })
						.then((l) => uniqueCommits(l.commits))
						.catch(() => [])
				)
			)
		]);
		const now = Date.now();
		return list.map((r, i) => {
			const days = Array.from({ length: 30 }, () => 0);
			for (const c of logs[i]) {
				const d = Math.floor((now - (toDate(c.commitTime)?.getTime() ?? 0)) / 86400000);
				if (d < 30) days[29 - d]++;
			}
			const lastRun: Run | undefined = runs.find((x) => x.trigger !== 'worktree' && x.repo === `${r.owner}/${r.name}`);
			return { repo: r, days, last: logs[i][0], lastRun };
		});
	});
	const navLink =
		'rounded-sm px-2 py-1 text-[0.8125rem] text-fg-muted hover:bg-surface-2 hover:text-fg aria-[current=page]:bg-accent-soft aria-[current=page]:text-accent aria-[current=page]:font-semibold';
</script>

<div class="flex h-dvh flex-col bg-bg text-fg">
	<a
		href="#main"
		class="sr-only z-50 rounded-sm bg-accent px-3 py-1.5 text-[0.8125rem] text-accent-fg focus:not-sr-only focus:absolute focus:top-1 focus:left-1"
		>Skip to content</a
	>
	<header class="shrink-0 border-b border-border bg-surface">
		<div class="flex h-9 items-center gap-3 pr-3 text-xs sm:pr-4">
			<button
				type="button"
				onclick={() => goto(drawerHref(!drawerOpen), { reset: false })}
				aria-expanded={drawerOpen}
				aria-controls={drawerOpen ? 'overview' : undefined}
				class="flex h-full min-w-0 flex-1 items-center gap-3 px-3 text-left text-fg-subtle hover:bg-surface-2 hover:text-fg sm:px-4"
			>
				<span class="w-3" aria-hidden="true">{drawerOpen ? '▾' : '▸'}</span>
				<span class="text-[0.8125rem] font-semibold tracking-tight text-fg">kajiba</span>
				{#if here.text}
					<span aria-hidden="true" class="truncate text-fg-muted {here.mono ? 'font-mono' : ''}"
						>{here.text}</span
					>
				{/if}
				<span class="ml-auto shrink-0 rounded-sm border border-border px-1.5 py-0.5 text-fg-muted"
					>{drawerOpen ? 'Close' : 'Open'} overview</span>
			</button>
			<div class="ml-auto flex items-center gap-1">
				<a
					href="/-/account"
					aria-current={isCurrentPage(page.url.pathname, '/-/account') ? 'page' : undefined}
					class="flex items-center gap-1.5 {navLink}"
					><Icon name="user" size={14} /><span class="sr-only sm:not-sr-only">{viewer.data?.user ?? 'Account'}</span
					></a
				>
				{#if viewer.data}
					<button
						type="button"
						onclick={signOut}
						class="rounded-sm p-1.5 text-fg-muted hover:bg-surface-2 hover:text-fg"
					>
						<Icon name="logout" size={14} label="Sign out" />
					</button>
				{/if}
				<ThemeToggle class="rounded-sm p-1.5 text-fg-muted hover:bg-surface-2" />
			</div>
		</div>
	</header>

	{#if drawerOpen}
		<!-- Unfolded: the forge overview takes the whole screen; picking anything folds it. -->
		<section
			id="overview"
			class="min-h-0 flex-1 overflow-y-auto bg-surface"
			aria-label="kajiba overview"
		>
			<div
				class="grid content-start gap-8 px-3 py-6 sm:px-6 lg:grid-cols-[minmax(0,1fr)_14rem]"
			>
				<section aria-label="Repositories">
					<h2 class="mb-2 text-xs tracking-wide text-fg-subtle uppercase">Repositories</h2>
					<ul class="divide-y divide-border/70">
						{#each overview.data ?? [] as o (o.repo.publicId)}
							{@const max = Math.max(1, ...o.days)}
							{@const key = `${o.repo.owner}/${o.repo.name}`}
							<li>
								<a
									href="/{key}"
									aria-current={key === repoKey ? 'page' : undefined}
									class="grid grid-cols-[minmax(0,10rem)_1fr_auto] items-center gap-4 py-1.5 text-[0.8125rem] hover:bg-surface-2 aria-[current=page]:bg-accent-soft aria-[current=page]:text-accent aria-[current=page]:font-semibold sm:grid-cols-[minmax(0,12rem)_8rem_1fr_auto]"
								>
									<span
										class="truncate font-mono text-xs {o.repo.archived
											? 'text-fg-subtle'
											: key === repoKey
												? 'text-accent'
												: ''}">{key}</span
									>
									<span class="hidden h-4 items-end gap-px sm:flex" aria-hidden="true">
										{#each o.days as d, i (i)}<span
												class="w-[3px] rounded-[1px] {d ? 'bg-accent/70' : 'bg-border'}"
												style:height="{d ? Math.max(25, (d / max) * 100) : 15}%"
											></span>{/each}
									</span>
									<span class="truncate text-xs text-fg-subtle"
										>{o.last
											? `last change ${relativeTime(toDate(o.last.commitTime))}`
											: 'empty'}</span
									>
									<span class="w-4"
										>{#if o.lastRun}<RunBadge state={o.lastRun.state} size={11} />{/if}</span
									>
								</a>
							</li>
						{/each}
					</ul>
				</section>
				<nav aria-label="Pages">
					<h2 class="mb-2 text-xs tracking-wide text-fg-subtle uppercase">Pages</h2>
					<ul class="space-y-0.5">
						{#each PAGES as p (p.href)}<li>
								<a
									href={p.href}
									aria-current={isCurrentPage(page.url.pathname, p.href) ? 'page' : undefined}
									class="flex items-center gap-2 {navLink}"
									><Icon name={p.icon} size={13} />{p.label}</a
								>
							</li>{/each}
					</ul>
				</nav>
			</div>
		</section>
	{:else}
		<main id="main" tabindex="-1" class="flex min-h-0 flex-1 flex-col outline-none">
			{#if viewer.error && isUnauthenticated}
				<StateView error={viewer.error} />
			{:else}
				{@render children()}
			{/if}
		</main>
	{/if}
</div>
