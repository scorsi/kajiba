<!--
	Administration (runners included), in the bands grammar of the repository
	view and Activity. Everything here is declared in the server configuration
	(infra), so the page only shows state — no edit controls.

	  Pulse     — server identity, state line, needs attention, server facts
	  Runners   — grouped by host: state, labels, what it runs now, 7-day figures
	  Mirrors   — push mirrors (open when one fails)
	  Users     — users and their roles across repositories (folded)
	  History   — operational events: failed runs, mirrors, runners (folded)

	Band state lives in the URL: runners, mirrors, users, history, kind.
-->
<script lang="ts">
	import RunRef from '#lib/components/RunRef.svelte';
	import { page } from '$app/state';
	import {
		MirrorState,
		Role,
		RunState,
		RunnerState,
		type Runner,
		type ServerInfo
	} from '#lib/api.ts';
	import Icon from '#lib/components/Icon.svelte';
	import RunnerStateDot from '#lib/components/RunnerState.svelte';
	import { absoluteTime, formatBytes, relativeTime, shortId, toDate } from '#lib/format.ts';
	import RunBadge, { RUN_LABEL } from '#lib/components/RunBadge.svelte';
	import BandStrip from '#lib/components/BandStrip.svelte';
	import { buildEvents, countEvents } from '#lib/data/events.ts';
	import { dur, repoHref, type Forge } from '#lib/data/forge.ts';
	import HistoryFeed from '#lib/components/HistoryFeed.svelte';
	import NeedsAttention from '#lib/components/NeedsAttention.svelte';

	let { forge, server }: { forge: Forge; server: ServerInfo } = $props();

	const q = $derived(page.url.searchParams);
	const pulseOpen = $derived(q.get('pulse') !== '0');
	function hrefFor(next: Record<string, string | null>) {
		const u = new URLSearchParams(q.toString());
		for (const [k, v] of Object.entries(next)) v === null ? u.delete(k) : u.set(k, v);
		return `?${u}`;
	}

	// ── runners ────────────────────────────────────────────────────────
	const now = Date.now() / 1000;
	const secs = (t?: { seconds: bigint }) => Number(t?.seconds ?? 0n);
	const median = (xs: number[]) => {
		if (!xs.length) return 0;
		const s = [...xs].sort((a, b) => a - b);
		return s[Math.floor(s.length / 2)];
	};
	const runsById = $derived(new Map(forge.runs.map((r) => [r.id, r])));
	const queued = $derived(forge.runs.filter((r) => r.state === RunState.QUEUED));
	const online = $derived(forge.runners.filter((r) => r.state !== RunnerState.OFFLINE));
	const busy = $derived(forge.runners.filter((r) => r.state === RunnerState.BUSY));

	function stats(r: Runner) {
		const runs = forge.runs.filter(
			(x) => x.runner === r.name && secs(x.createTime) > now - 7 * 86400
		);
		return {
			runs,
			failed: runs.filter((x) => x.state === RunState.FAILED).length,
			median: median(runs.filter((x) => x.state !== RunState.RUNNING).map((x) => x.durationSeconds))
		};
	}
	const hosts = $derived.by(() => {
		const m = new Map<string, Runner[]>();
		for (const r of forge.runners) m.set(r.host, [...(m.get(r.host) ?? []), r]);
		return [...m].map(([host, runners]) => ({
			host,
			runners,
			systems: [
				...new Set(runners.flatMap((r) => r.labels.filter((l) => /-(linux|darwin)$/.test(l))))
			]
		}));
	});

	// ── CI, 7 days ─────────────────────────────────────────────────────
	const runs7 = $derived(forge.runs.filter((r) => secs(r.createTime) > now - 7 * 86400));
	const passed7 = $derived(runs7.filter((r) => r.state === RunState.SUCCEEDED).length);
	const median7 = $derived(
		median(runs7.filter((r) => r.state !== RunState.RUNNING).map((r) => r.durationSeconds))
	);

	// ── mirrors ────────────────────────────────────────────────────────
	const mirrorsFailing = $derived(forge.mirrors.filter((m) => m.state === MirrorState.FAILING));
	const mirrorsOk = $derived(forge.mirrors.filter((m) => m.state === MirrorState.OK).length);
	const mirrorsOff = $derived(forge.mirrors.filter((m) => m.state === MirrorState.DISABLED).length);
	const MIRROR = {
		[MirrorState.UNSPECIFIED]: { label: 'unknown', icon: 'mirror', cls: 'text-fg-subtle' },
		[MirrorState.OK]: { label: 'ok', icon: 'check', cls: 'text-ok' },
		[MirrorState.FAILING]: { label: 'failing', icon: 'alert', cls: 'text-danger' },
		[MirrorState.DISABLED]: { label: 'disabled', icon: 'x', cls: 'text-fg-subtle' }
	} as const;

	// ── users ──────────────────────────────────────────────────────────
	const users = $derived(
		server.users.map(({ name: user, admin: isAdmin, disabled }) => {
			const roles = { admin: 0, write: 0, read: 0 };
			for (const { repo } of forge.repos) {
				const p = repo.permissions.find((p) => p.user === user);
				if (p?.role === Role.ADMIN) roles.admin++;
				else if (p?.role === Role.WRITE) roles.write++;
				else if (p?.role === Role.READ) roles.read++;
			}
			return {
				user,
				isAdmin,
				disabled,
				...roles,
				owned: forge.repos.filter((r) => r.repo.owner === user).length
			};
		})
	);

	// ── bands ──────────────────────────────────────────────────────────
	const runnersOpen = $derived(q.get('runners') !== '0');
	const mirrorsOpen = $derived(
		q.get('mirrors') === '1' || (!!mirrorsFailing.length && q.get('mirrors') !== '0')
	);
	const usersOpen = $derived(q.get('users') === '1');
	const historyOpen = $derived(q.get('history') === '1');
	const toggle = (param: string, open: boolean, defaultOpen: boolean) =>
		hrefFor({ [param]: open ? (defaultOpen ? '0' : null) : defaultOpen ? null : '1' });

	// History: operational events only (no commits, no releases)
	const events = $derived(
		buildEvents(
			forge.repos.map((r) => ({ repo: r.key, commits: [], tags: new Map(), runs: r.runs })),
			{ mirrors: forge.mirrors, runners: forge.runners, since: now - 30 * 86400 }
		)
	);
	const counts = $derived(countEvents(events));

	const stepClass = (s: RunState) =>
		s === RunState.SUCCEEDED
			? 'bg-ok'
			: s === RunState.RUNNING
				? 'bg-accent motion-safe:animate-pulse'
				: s === RunState.FAILED
					? 'bg-danger'
					: 'bg-border';
</script>

<div class="relative min-h-0 flex-1 overflow-y-auto">
	<!-- Pulse -->
	<BandStrip label="Pulse" open={pulseOpen} href={hrefFor({ pulse: pulseOpen ? '0' : null })}>
		<span>Administration</span>
	</BandStrip>
	{#if pulseOpen}
	<section class="border-b border-border bg-surface px-4 py-4 sm:px-6" aria-label="Pulse">
		<div class="grid grid-cols-[minmax(0,1fr)] gap-x-10 gap-y-5 lg:grid-cols-[minmax(0,1fr)_17rem]">
			<div class="min-w-0">
				<h1 class="mt-1 text-2xl font-semibold tracking-tight">
					Administration <span class="text-base font-normal text-fg-muted"
						>kajiba {server.version}</span
					>
				</h1>
				<p class="mt-1 text-xs text-fg-subtle">
					<Icon name="lock" size={12} class="mr-1 inline-block align-[-2px]" />Read-only. Runners,
					mirrors, users and tokens are declared in
					<span class="font-mono text-fg-muted">generation {server.configGeneration}</span>
				</p>
				<p class="mt-3 flex flex-wrap items-center gap-x-4 gap-y-1 text-xs text-fg-subtle">
					<span class="flex items-center gap-1.5"
						>{#each forge.runners as r (r.name)}<RunnerStateDot state={r.state} />{/each}
						<span
							><span class="tabular text-fg-muted">{online.length}/{forge.runners.length}</span> runners
							online</span
						></span
					>
					<span><span class="tabular text-fg-muted">{busy.length}</span> busy</span>
					<span
						>queue {#if queued.length}<span class="tabular text-fg-muted">{queued.length}</span
							>{:else}empty{/if}</span
					>
					<span class="flex items-center gap-1.5"
						><RunBadge state={RunState.SUCCEEDED} size={10} />CI {passed7}/{runs7.length} passed · median
						{dur(median7)} · 7 days</span
					>
					<span class="flex items-center gap-1.5 {mirrorsFailing.length ? 'text-danger' : ''}"
						><Icon name="mirror" size={11} />mirrors {mirrorsOk} ok{mirrorsFailing.length
							? ` · ${mirrorsFailing.length} failing`
							: ''}</span
					>
					<span
						><span class="tabular text-fg-muted">{server.users.length}</span> users ·
						<span class="tabular text-fg-muted">{server.declaredTokens}</span> declared tokens</span
					>
				</p>
				<NeedsAttention
					{forge}
					runnerHref={(name) => `${hrefFor({ runners: null })}#runner-${name}`}
					mirrorHref={() => `${hrefFor({ mirrors: '1' })}#mirrors`}
				/>
			</div>

			<!-- Server -->
			<aside aria-label="Server">
				<h2 class="mb-2 text-xs tracking-wide text-fg-subtle uppercase">Server</h2>
				<dl class="grid grid-cols-[auto_minmax(0,1fr)] gap-x-4 gap-y-1.5 text-[0.8125rem]">
					<dt class="text-fg-subtle">Version</dt>
					<dd>{server.version}</dd>
					<dt class="text-fg-subtle">Up since</dt>
					<dd title={absoluteTime(toDate(server.startTime))}>
						{relativeTime(toDate(server.startTime))}
					</dd>
					<dt class="text-fg-subtle">Config</dt>
					<dd class="tabular">generation {server.configGeneration}</dd>
					<dt class="text-fg-subtle">Repositories</dt>
					<dd class="tabular">
						{forge.repos.length}
						<span class="text-fg-subtle">· {formatBytes(server.storageBytes)}</span>
					</dd>
					<dt class="text-fg-subtle">Database</dt>
					<dd class="tabular">SQLite · {formatBytes(server.databaseBytes)}</dd>
				</dl>
			</aside>
		</div>
	</section>
	{/if}

	<!-- Runners -->
	<BandStrip label="Runners" open={runnersOpen} href={toggle('runners', runnersOpen, true)}>
		<span class="tabular">{online.length}/{forge.runners.length} online</span>
		{#each busy as r (r.name)}{#if r.currentRun}<span class="flex items-center gap-1.5"
					><RunBadge state={RunState.RUNNING} size={10} /><span class="font-mono">{r.name}</span>
					#{r.currentRun.short}<RunRef run={r.currentRun.run} /></span
				>{/if}{/each}
	</BandStrip>
	{#if runnersOpen}
		<section id="runners" class="scroll-mt-10 border-b border-border" aria-label="Runners">
			<div
				class="hidden grid-cols-[6rem_9rem_minmax(0,1fr)_minmax(0,1.4fr)_15rem] gap-x-5 border-b border-border/60 px-6 py-1.5 text-xs text-fg-subtle lg:grid"
				aria-hidden="true"
			>
				<span>State</span><span>Runner</span><span>Labels</span><span>Now</span><span
					>Last 7 days</span
				>
			</div>
			{#each hosts as h (h.host)}
				<div class="border-b border-border/60 last:border-0">
					<h3
						class="flex items-baseline gap-2 bg-surface/60 px-4 pt-2.5 pb-1 text-xs text-fg-subtle sm:px-6"
					>
						<Icon name="server" size={12} class="self-center" /><span
							class="font-mono text-[0.8125rem] text-fg">{h.host || 'not connected yet'}</span
						>
						{h.runners.length} runner{h.runners.length > 1 ? 's' : ''} · {h.systems.join(', ')}
					</h3>
					<ul>
						{#each h.runners as r (r.name)}
							{@const s = stats(r)}
							{@const run = r.currentRun ? runsById.get(r.currentRun.id) : undefined}
							<li
								id="runner-{r.name}"
								class="grid scroll-mt-10 grid-cols-[minmax(0,1fr)] gap-x-5 gap-y-1.5 px-4 py-2.5 text-[0.8125rem] target:bg-accent-soft sm:px-6 lg:grid-cols-[6rem_9rem_minmax(0,1fr)_minmax(0,1.4fr)_15rem] lg:items-center"
							>
								<span class="flex items-center gap-3 lg:contents">
									<span class={r.state === RunnerState.OFFLINE ? 'text-fg-subtle' : 'text-fg-muted'}
										><RunnerStateDot state={r.state} label /></span
									>
									<span class="min-w-0">
										<span
											class="font-mono {r.state === RunnerState.OFFLINE
												? 'text-fg-muted'
												: 'font-medium'}">{r.name}</span
										>
										<span class="block text-xs text-fg-subtle"
											>{r.backend}{#if r.disabled}{' · '}removed from the configuration{/if}</span
										>
									</span>
								</span>
								<span class="flex flex-wrap gap-1">
									{#each r.labels as l (l)}
										<span
											class="rounded-sm bg-surface-2 px-1 font-mono text-[0.6875rem] text-fg-muted"
											>{l}</span
										>
									{/each}
								</span>
								<span class="min-w-0 text-xs">
									{#if r.state === RunnerState.BUSY && r.currentRun}
										<a
											href="/-/runs/{r.currentRun.id}"
											class="flex items-center gap-1.5 hover:underline"
											><RunBadge state={RunState.RUNNING} size={10} /><span class="font-mono"
												>#{r.currentRun.short}</span
											><span class="truncate font-mono text-fg-muted">{r.currentRun.repo}</span><RunRef run={r.currentRun.run} />
											<span class="tabular ml-auto text-fg-subtle"
												>{relativeTime(toDate(r.currentRun.startTime))}</span
											></a
										>
										{#if run}
											<span
												class="mt-1.5 flex gap-0.5"
												role="img"
												aria-label="Steps of run #{run.short}"
												>{#each run.steps as st, si (si)}<span
														class="h-1 flex-1 rounded-full {stepClass(st.state)}"
														title="{st.name} · {RUN_LABEL[st.state]}"
													></span>{/each}</span
											>
										{/if}
									{:else if r.state === RunnerState.OFFLINE}
										<span class="text-fg-muted" title={absoluteTime(toDate(r.lastSeenTime))}
											>offline · last seen {relativeTime(toDate(r.lastSeenTime))}</span
										>
									{:else}
										<span class="text-fg-subtle"
											>idle{#if s.runs[0]}{' · last run '}
												<a href="/-/runs/{s.runs[0].id}" class="font-mono hover:underline"
													>#{s.runs[0].short}</a
												>
												<RunRef run={s.runs[0]} />{relativeTime(toDate(s.runs[0].createTime))}{/if}</span
										>
									{/if}
								</span>
								<span class="flex items-end gap-3 text-xs text-fg-subtle">
									<span class="tabular whitespace-nowrap"
										>{s.runs.length} runs{#if s.failed}<span class="text-danger"
												>{` · ${s.failed} failed`}</span
											>{/if}{#if s.median}
											{` · ${dur(s.median)}`}{/if}</span
									>
								</span>
							</li>
						{/each}
					</ul>
				</div>
			{/each}
		</section>
	{/if}

	<!-- Mirrors -->
	<BandStrip
		label="Mirrors"
		open={mirrorsOpen}
		href={toggle('mirrors', mirrorsOpen, !!mirrorsFailing.length)}
	>
		<span class="tabular">{mirrorsOk} ok</span>
		{#if mirrorsFailing.length}<span class="tabular text-danger"
				>{mirrorsFailing.length} failing</span
			>{/if}
		{#if mirrorsOff}<span class="tabular">{mirrorsOff} disabled</span>{/if}
	</BandStrip>
	{#if mirrorsOpen}
		<section id="mirrors" class="scroll-mt-10 border-b border-border" aria-label="Mirrors">
			<ul class="divide-y divide-border/60">
				{#each forge.mirrors as m (m.repo + m.remoteUrl)}
					{@const s = MIRROR[m.state]}
					<li class="px-4 py-2.5 text-[0.8125rem] sm:px-6">
						<div class="flex flex-wrap items-baseline gap-x-4 gap-y-1">
							<span class="inline-flex w-20 items-center gap-1.5 text-xs {s.cls}"
								><Icon name={s.icon} size={12} />{s.label}</span
							>
							<a href={repoHref(m.repo)} class="font-mono text-xs hover:underline">{m.repo}</a>
							<span class="min-w-0 truncate font-mono text-xs text-fg-subtle">→ {m.remoteUrl}</span>
							<span class="ml-auto text-xs text-fg-subtle">
								{#if m.lastSuccessTime}last push {relativeTime(
										toDate(m.lastSuccessTime)
									)}{:else}never pushed{/if}
							</span>
						</div>
						{#if m.state === MirrorState.FAILING}
							<div class="mt-1.5 rounded-sm bg-danger-soft px-2 py-1.5 text-xs sm:ml-24">
								<code class="break-words text-danger">{m.lastError}</code>
								<p class="mt-0.5 text-fg-muted">
									{m.attempts} failed attempts · next retry {relativeTime(
										toDate(m.nextAttemptTime)
									)}
								</p>
							</div>
						{/if}
					</li>
				{:else}
					<li class="px-4 py-3 text-[0.8125rem] text-fg-subtle sm:px-6">No mirror declared.</li>
				{/each}
			</ul>
		</section>
	{/if}

	<!-- Users -->
	<BandStrip label="Users" open={usersOpen} href={toggle('users', usersOpen, false)}>
		<span class="font-mono">{server.users.map((u) => u.name).join(' · ')}</span>
		<span class="tabular">{server.declaredTokens} declared tokens</span>
	</BandStrip>
	{#if usersOpen}
		<section id="users" class="border-b border-border" aria-label="Users">
			<div class="overflow-x-auto">
				<table class="w-full max-w-3xl text-[0.8125rem]">
					<caption class="sr-only">Users and their roles across repositories</caption>
					<thead class="text-xs text-fg-subtle">
						<tr>
							<th class="px-4 py-1.5 text-left font-normal sm:px-6">User</th>
							<th class="px-2 py-1.5 text-right font-normal">Owns</th>
							<th class="px-2 py-1.5 text-right font-normal">Admin</th>
							<th class="px-2 py-1.5 text-right font-normal">Write</th>
							<th class="px-4 py-1.5 text-right font-normal">Read</th>
						</tr>
					</thead>
					<tbody class="tabular">
						{#each users as u (u.user)}
							<tr class="border-t border-border/60">
								<td class="px-4 py-1.5 font-mono sm:px-6"
									>{u.user}{#if u.isAdmin}<span class="ml-2 font-sans text-xs text-fg-subtle"
											>administrator</span
										>{/if}{#if u.disabled}<span class="ml-2 font-sans text-xs text-fg-subtle"
											>disabled</span
										>{/if}</td
								>
								<td class="px-2 py-1.5 text-right text-fg-muted">{u.owned}</td>
								<td class="px-2 py-1.5 text-right text-fg-muted">{u.admin}</td>
								<td class="px-2 py-1.5 text-right text-fg-muted">{u.write}</td>
								<td class="px-4 py-1.5 text-right text-fg-muted">{u.read}</td>
							</tr>
						{/each}
					</tbody>
				</table>
			</div>
			<p class="px-4 pt-1 pb-3 text-xs text-fg-subtle sm:px-6">
				Repositories per role. Access, passkey enrolment and declared tokens come from the server
				configuration.
			</p>
		</section>
	{/if}

	<!-- History -->
	<BandStrip label="History" open={historyOpen} href={toggle('history', historyOpen, false)}>
		{#if counts.ci}<span class="tabular text-danger"
				>{counts.ci} failed run{counts.ci > 1 ? 's' : ''}</span
			>{/if}
		{#if counts.mirror}<span class="tabular text-danger"
				>{counts.mirror} mirror{counts.mirror > 1 ? 's' : ''} failing</span
			>{/if}
		{#if counts.runner}<span class="tabular"
				>{counts.runner} runner{counts.runner > 1 ? 's' : ''} offline</span
			>{/if}
		<span>· 30 days</span>
	</BandStrip>
	{#if historyOpen}
		<section
			class="relative max-h-[55vh] overflow-y-auto border-b border-border bg-surface"
			aria-label="History"
		>
			<HistoryFeed
				{events}
				runByCommit={new Map()}
				commitHref={(repo, id) => repoHref(repo, { at: id })}
				repoHref={(repo) => repoHref(repo)}
				repos={forge.repos.map((r) => r.key)}
			/>
		</section>
	{/if}
</div>
