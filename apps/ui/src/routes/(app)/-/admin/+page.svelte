<!--
	Administration: read-only view of what the server configuration declares
	(runners, mirrors, users) and how it is doing right now.
-->
<script lang="ts">
	import {
		Code,
		ApiError,
		MirrorState,
		RunnerState,
		Role,
		getServerInfo,
		getViewer,
		listMirrorStatus,
		listRunners
	} from '#lib/api.ts';
	import Icon from '#lib/components/Icon.svelte';
	import RunnerStateDot from '#lib/components/RunnerState.svelte';
	import Skeleton from '#lib/components/Skeleton.svelte';
	import StateView from '#lib/components/StateView.svelte';
	import { absoluteTime, formatBytes, relativeTime, shortId, toDate } from '#lib/format.ts';
	import { allRepos } from '#lib/loaders.ts';
	import { resource } from '#lib/resource.svelte.ts';

	const data = resource(async () => {
		const viewer = await getViewer();
		if (!viewer.admin) throw new ApiError(Code.PermissionDenied, 'administrators only');
		const [runners, mirrors, server, repos] = await Promise.all([
			listRunners(),
			listMirrorStatus(),
			getServerInfo(),
			allRepos()
		]);
		return { runners: runners.runners, mirrors: mirrors.mirrors, server, repos };
	});

	const d = $derived(data.data);
	const online = $derived(d?.runners.filter((r) => r.state !== RunnerState.OFFLINE).length ?? 0);
	const busy = $derived(d?.runners.filter((r) => r.state === RunnerState.BUSY).length ?? 0);
	const failing = $derived(d?.mirrors.filter((m) => m.state === MirrorState.FAILING) ?? []);

	/** Users with the number of repositories per role, derived from repo permissions. */
	const users = $derived.by(() => {
		if (!d) return [];
		return d.server.users.map((user) => {
			const roles = { admin: 0, write: 0, read: 0 };
			for (const r of d.repos) {
				const p = r.permissions.find((p) => p.user === user);
				if (p?.role === Role.ADMIN) roles.admin++;
				else if (p?.role === Role.WRITE) roles.write++;
				else if (p?.role === Role.READ) roles.read++;
			}
			return { user, ...roles, owned: d.repos.filter((r) => r.owner === user).length };
		});
	});

	const MIRROR = {
		[MirrorState.UNSPECIFIED]: { label: 'unknown', icon: 'mirror', cls: 'text-fg-subtle' },
		[MirrorState.OK]: { label: 'ok', icon: 'check', cls: 'text-ok' },
		[MirrorState.FAILING]: { label: 'failing', icon: 'alert', cls: 'text-danger' },
		[MirrorState.DISABLED]: { label: 'disabled', icon: 'x', cls: 'text-fg-subtle' }
	} as const;

	const h2 = 'text-[0.8125rem] font-medium';
</script>

<svelte:head><title>Administration · kajiba</title></svelte:head>

<main class="mx-auto max-w-6xl px-4 pt-5 pb-16 sm:px-6 lg:px-8 lg:pt-7">
	<header>
		<h1 class="text-xl font-semibold tracking-tight">Administration</h1>
		<p class="mt-1 flex items-center gap-1.5 text-[0.8125rem] text-fg-muted">
			<Icon name="lock" size={13} /> Read-only. Runners, mirrors, users and tokens are declared in the
			server configuration.
		</p>
	</header>

	{#if data.error}
		<StateView error={data.error} onretry={data.retry} what="page" />
	{:else if !d}
		<div class="mt-6 rounded-md border border-border bg-surface p-5"><Skeleton lines={12} /></div>
	{:else}
		{#if failing.length}
			<p
				class="mt-5 flex items-start gap-2 rounded-md border border-danger/30 bg-danger-soft px-3 py-2 text-[0.8125rem] text-danger"
				role="status"
			>
				<Icon name="alert" size={15} class="mt-0.5" />
				<span
					>{failing.length} mirror{failing.length > 1 ? 's' : ''} failing:
					{failing.map((m) => m.repo).join(', ')}.
					<a href="#mirrors" class="underline">See mirrors</a></span
				>
			</p>
		{/if}

		<!-- Runners -->
		<section id="runners" class="mt-6 scroll-mt-16 rounded-md border border-border bg-surface">
			<div class="flex items-baseline justify-between border-b border-border px-4 py-3">
				<h2 class={h2}>Runners</h2>
				<span class="tabular text-xs text-fg-subtle"
					>{online}/{d.runners.length} online · {busy} busy</span
				>
			</div>
			<div
				class="hidden grid-cols-[7rem_10rem_6rem_minmax(0,1fr)_minmax(0,1.3fr)] gap-x-4 border-b border-border px-4 py-1.5 text-xs text-fg-subtle md:grid"
				aria-hidden="true"
			>
				<span>State</span><span>Runner</span><span>Backend</span><span>Labels</span><span
					>Activity</span
				>
			</div>
			<ul class="divide-y divide-border">
				{#each d.runners as r (r.name)}
					<li
						class="grid grid-cols-[auto_minmax(0,1fr)] gap-x-3 gap-y-1 px-4 py-2.5 text-[0.8125rem] md:grid-cols-[7rem_10rem_6rem_minmax(0,1fr)_minmax(0,1.3fr)] md:items-center md:gap-x-4"
					>
						<span class="text-fg-muted"><RunnerStateDot state={r.state} label /></span>
						<span class="min-w-0">
							<span class="font-mono font-medium">{r.name}</span>
							<span class="block text-xs text-fg-subtle">on {r.host}</span>
						</span>
						<span class="col-start-2 text-fg-muted md:col-start-auto">{r.backend}</span>
						<span class="col-start-2 flex flex-wrap gap-1 md:col-start-auto">
							{#each r.labels as l (l)}
								<span class="rounded-sm bg-surface-2 px-1 font-mono text-[0.6875rem] text-fg-muted"
									>{l}</span
								>
							{/each}
						</span>
						<span class="col-start-2 text-xs md:col-start-auto">
							{#if r.state === RunnerState.BUSY && r.currentRun}
								<a href="/-/runs/{r.currentRun.id}" class="text-link hover:underline"
									>run #{r.currentRun.id} · {r.currentRun.repo}</a
								>
								<span class="text-fg-subtle"
									>· started {relativeTime(toDate(r.currentRun.startTime))}</span
								>
							{:else}
								<span class="text-fg-subtle" title={absoluteTime(toDate(r.lastSeenTime))}
									>last seen {relativeTime(toDate(r.lastSeenTime))}</span
								>
							{/if}
						</span>
					</li>
				{/each}
			</ul>
		</section>

		<!-- Mirrors -->
		<section id="mirrors" class="mt-6 scroll-mt-16 rounded-md border border-border bg-surface">
			<div class="flex items-baseline justify-between border-b border-border px-4 py-3">
				<h2 class={h2}>Push mirrors</h2>
				<span class="text-xs text-fg-subtle">retried with backoff, alert on failure</span>
			</div>
			<ul class="divide-y divide-border">
				{#each d.mirrors as m (m.repo + m.remoteUrl)}
					{@const s = MIRROR[m.state]}
					<li class="px-4 py-2.5 text-[0.8125rem]">
						<div class="flex flex-wrap items-baseline gap-x-3 gap-y-1">
							<span class="inline-flex w-20 items-center gap-1.5 {s.cls}"
								><Icon name={s.icon} size={13} />{s.label}</span
							>
							<a href="/{m.repo}" class="font-medium hover:underline">{m.repo}</a>
							<span class="min-w-0 truncate font-mono text-xs text-fg-muted">→ {m.remoteUrl}</span>
							<span class="ml-auto text-xs text-fg-subtle">
								{#if m.lastSuccessTime}last push {relativeTime(
										toDate(m.lastSuccessTime)
									)}{:else}never pushed{/if}
							</span>
						</div>
						{#if m.state === MirrorState.FAILING}
							<div class="mt-1.5 rounded-sm bg-danger-soft px-2 py-1.5 text-xs sm:ml-[5.75rem]">
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
					<li class="px-4 py-4 text-[0.8125rem] text-fg-subtle">No mirror declared.</li>
				{/each}
			</ul>
		</section>

		<div class="mt-6 grid gap-6 lg:grid-cols-2">
			<!-- Users -->
			<section id="users" class="scroll-mt-16 rounded-md border border-border bg-surface">
				<h2 class="border-b border-border px-4 py-3 {h2}">Users</h2>
				<table class="w-full text-[0.8125rem]">
					<thead class="text-xs text-fg-subtle">
						<tr>
							<th class="px-4 py-1.5 text-left font-normal">User</th>
							<th class="px-2 py-1.5 text-right font-normal">Owns</th>
							<th class="px-2 py-1.5 text-right font-normal">Admin</th>
							<th class="px-2 py-1.5 text-right font-normal">Write</th>
							<th class="px-4 py-1.5 text-right font-normal">Read</th>
						</tr>
					</thead>
					<tbody class="tabular">
						{#each users as u (u.user)}
							<tr class="border-t border-border">
								<td class="px-4 py-1.5 font-medium">{u.user}</td>
								<td class="px-2 py-1.5 text-right text-fg-muted">{u.owned}</td>
								<td class="px-2 py-1.5 text-right text-fg-muted">{u.admin}</td>
								<td class="px-2 py-1.5 text-right text-fg-muted">{u.write}</td>
								<td class="px-4 py-1.5 text-right text-fg-muted">{u.read}</td>
							</tr>
						{/each}
					</tbody>
				</table>
				<p class="border-t border-border px-4 py-2 text-xs text-fg-subtle">
					Repository counts per role. {d.server.declaredTokens} declared tokens.
				</p>
			</section>

			<!-- Server -->
			<section id="server" class="scroll-mt-16 rounded-md border border-border bg-surface">
				<h2 class="border-b border-border px-4 py-3 {h2}">Server</h2>
				<dl
					class="grid grid-cols-[auto_minmax(0,1fr)] gap-x-6 gap-y-1.5 px-4 py-3 text-[0.8125rem]"
				>
					<dt class="text-fg-subtle">Version</dt>
					<dd>
						{d.server.version}
						<span class="font-mono text-xs text-fg-subtle">{shortId(d.server.commit)}</span>
					</dd>
					<dt class="text-fg-subtle">Up since</dt>
					<dd title={absoluteTime(toDate(d.server.startTime))}>
						{relativeTime(toDate(d.server.startTime))}
					</dd>
					<dt class="text-fg-subtle">Configuration</dt>
					<dd class="truncate font-mono text-xs leading-5">{d.server.configGeneration}</dd>
					<dt class="text-fg-subtle">Repositories</dt>
					<dd class="tabular">
						{d.repos.length}
						<span class="text-fg-subtle">· {formatBytes(d.server.storageBytes)}</span>
					</dd>
					<dt class="text-fg-subtle">Database</dt>
					<dd class="tabular">SQLite · {formatBytes(d.server.databaseBytes)}</dd>
				</dl>
			</section>
		</div>
	{/if}
</main>
