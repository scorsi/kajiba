<!--
	Repository view, /<owner>/<name>. README first; what happened and how the CI is
	doing sit above it in foldable bands; the file columns appear only when asked
	for (Files band) or when a path is selected.

	Bands (fold to a one-line strip, the horizontal twin of the column spines):
	  Pulse      — identity, state line, activity ribbon (scoped to the selected
	               path), latest commits, branches & releases
	  Pipelines  — current run with its steps, recent runs (when CI is on)
	  History    — events of the scope by day: commits, releases, failed and active runs,
	               mirror failures (folded by default; shared with Activity)
	  Files      — column browser (folded by default on the repo root)
	State lives in the URL: path, ref, at (time travel), pulse, pipelines,
	history, files, kind (History filter).
-->
<script lang="ts">
	import { preferences } from '#lib/viewer-preferences.svelte.ts';
	import RunActions from '#lib/components/RunActions.svelte';
	import { runLabel } from '#lib/run-label.ts';
	import RunRef from '#lib/components/RunRef.svelte';
	import DurationBar from '#lib/components/DurationBar.svelte';
	import { dayKey, lastDays, hasActiveRuns, uniqueCommits } from '#lib/data/forge.ts';
	import { goto } from '$app/navigation';
	import { page } from '$app/state';
	import { tick, type Snippet } from 'svelte';
	import BandStrip from '#lib/components/BandStrip.svelte';
	import {
		ChangeKind,
		Code,
		EntryKind,
		CiMode,
		MirrorState,
		RunState,
		getBlob,
		getCommitDiff,
		getTree,
		getViewer,
		canManageRuns,
		listCommits,
		listMirrorStatus,
		listRefs,
		listRuns,
		type Commit,
		type Run,
		type TreeEntry
	} from '#lib/api.ts';
	import { repoQuery, isMarkdown, localDateTime, fileViewMode, touchedPaths } from '#lib/repo-navigation.ts';
	import CopyButton from '#lib/components/CopyButton.svelte';
	import CloneButton from '#lib/components/CloneButton.svelte';
	import DiffView from '#lib/components/DiffView.svelte';
	import CodeView from '#lib/components/CodeView.svelte';
	import Icon, { type IconName } from '#lib/components/Icon.svelte';
	import Markdown from '#lib/components/Markdown.svelte';
	import RefSelect from '#lib/components/RefSelect.svelte';
	import { lineDiff } from '#lib/diff.ts';
	import {
		README_NAMES,
		commitBody,
		commitSubject,
		formatBytes,
		isBinary,
		refShortName,
		relativeTime,
		shortId,
		toDate
	} from '#lib/format.ts';
	import { languageFor } from '#lib/highlight.ts';
	import { allRepos, sortEntries } from '#lib/loaders.ts';
	import { ENTRY_ICON, ROLE_LABEL } from '#lib/repo-ui.ts';
	import { resource } from '#lib/resource.svelte.ts';
	import RunBadge, { RUN_LABEL } from '#lib/components/RunBadge.svelte';
	import { buildEvents, countEvents, visibleRuns } from '#lib/data/events.ts';
	import HistoryFeed from '#lib/components/HistoryFeed.svelte';

	let { repoKey }: { repoKey: string } = $props();

	// ── URL state ──────────────────────────────────────────────────────
	const q = $derived(page.url.searchParams);
	const path = $derived(q.get('path') ?? '');
	const at = $derived(q.get('at') ?? '');
	const segs = $derived(path.split('/').filter(Boolean));
	const pulseOpen = $derived(q.get('pulse') !== '0');
	const historyOpen = $derived(q.get('history') === '1');
	const pipelinesOpen = $derived(q.get('pipelines') !== '0');
	const filesOpen = $derived(q.get('files') === '1' || (!!path && q.get('files') !== '0'));

	const hrefFor = (next: Record<string, string | null>) => repoQuery(q, next);
	const go = (next: Record<string, string | null>) => goto(hrefFor(next), { reset: false });

	// ── data ───────────────────────────────────────────────────────────
	const repos = resource(() => allRepos());
	const repo = $derived(repos.data?.find((r) => `${r.owner}/${r.name}` === repoKey));
	const ciOn = $derived(repo !== undefined && repo.ciMode > CiMode.OFF);
	const refs = resource(() => listRefs({ repo: repoKey }).then((r) => r.refs));
	const head = $derived(
		q.get('ref')
			? (refs.data?.find((r) => refShortName(r.name) === q.get('ref'))?.name ?? '')
			: (repo?.defaultBranch ?? '')
	);
	const empty = $derived(!!refs.data && !refs.data.some((r) => r.name === repo?.defaultBranch));
	const revision = $derived(at || head);
	const historyPath = $derived(at ? (q.get('scope') ?? path) : path);
	// Mirror state is for administrators (ADR 0139); the declared mirrors show for everyone.
	const viewer = resource(() => getViewer());
	const mirrors = resource(async () =>
		viewer.data?.admin
			? (await listMirrorStatus()).mirrors.filter((x) => x.repo === repoKey)
			: []
	);
	const runs = resource(
		() => listRuns({ repo: repoKey, pageSize: 60 }).then((r) => r.runs),
		hasActiveRuns
	);
	const branchRuns = $derived(
		visibleRuns(runs.data ?? [], preferences.hideFixedFailures).filter((r) => r.trigger !== 'worktree' && (!r.ref || r.ref === head))
	);
	const worktreeRuns = $derived((runs.data ?? []).filter((r) => r.trigger === 'worktree'));
	const runByCommit = $derived(new Map(branchRuns.map((r) => [r.commitId, r])));
	const durationMax = $derived(Math.max(1, ...(runs.data ?? []).map((r) => r.durationSeconds)));
	const current = $derived(branchRuns.find((r) => r.state === RunState.RUNNING) ?? branchRuns[0]);

	let logMore = $state(false);
	const log = resource(async () => {
		if (!head || empty) return [] as Commit[];
		const res = await listCommits({ repo: repoKey, revision: head, path: historyPath, want: 400 });
		logMore = !!res.nextPageToken;
		return uniqueCommits(res.commits);
	});

	const tagsAt = $derived.by(() => {
		const m = new Map<string, string[]>();
		for (const r of refs.data ?? [])
			if (r.name.startsWith('refs/tags/'))
				m.set(r.commitId, [...(m.get(r.commitId) ?? []), refShortName(r.name)]);
		return m;
	});

	// ── ribbon ─────────────────────────────────────────────────────────
	// The commit of a time-travel banner may not touch the filtered path: fetch it alone.
	const atFetched = resource(async () =>
		at ? (await listCommits({ repo: repoKey, revision: at, pageSize: 1 })).commits[0] : undefined
	);
	const atCommit = $derived(at ? (log.data?.find((c) => c.id === at) ?? atFetched.data) : undefined);
	const atDay = $derived(atCommit ? dayKey(toDate(atCommit.commitTime)!) : '');
	const WORST = [RunState.RUNNING, RunState.FAILED, RunState.CANCELLED, RunState.SUCCEEDED];
	const ribbon = $derived.by(() => {
		const byDay = new Map<string, Commit[]>();
		const flags = new Map<string, string[]>();
		for (const c of log.data ?? []) {
			const k = dayKey(toDate(c.commitTime)!);
			byDay.set(k, [...(byDay.get(k) ?? []), c]);
			const t = tagsAt.get(c.id);
			if (t) flags.set(k, [...(flags.get(k) ?? []), ...t]);
		}
		return lastDays(90).map(({ dt, k }) => {
			const commits = byDay.get(k) ?? [];
			const states = commits
				.map((c) => runByCommit.get(c.id)?.state)
				.filter((s) => s !== undefined);
			const ciState = WORST.find((s) => states.includes(s));
			return { k, dt, commits, tags: flags.get(k) ?? [], ciState };
		});
	});
	const ribbonMax = $derived(Math.max(1, ...ribbon.map((d) => d.commits.length)));
	const inWindow = $derived(ribbon.reduce((n, d) => n + d.commits.length, 0));
	const recent = $derived(
		q.get('day') ? (ribbon.find((d) => d.k === q.get('day'))?.commits ?? []) : (log.data ?? []).slice(0, 4)
	);

	// ── files ──────────────────────────────────────────────────────────
	const columns = resource(async () => {
		if (!revision || empty) return [];
		const out: { dir: string; entries: TreeEntry[] }[] = [];
		const dirs = [''];
		for (let i = 1; i <= segs.length; i++) dirs.push(segs.slice(0, i).join('/'));
		for (const dir of dirs) {
			try {
				out.push({
					dir,
					entries: sortEntries((await getTree({ repo: repoKey, revision, path: dir })).entries)
				});
			} catch {
				break;
			}
		}
		return out;
	});
	const selected = $derived.by(() => {
		if (!segs.length) return undefined;
		const parent = columns.data?.find((c) => c.dir === segs.slice(0, -1).join('/'));
		return parent?.entries.find((e) => e.name === segs.at(-1));
	});
	const isFile = $derived(
		!!selected && selected.kind !== EntryKind.TREE && selected.kind !== EntryKind.SUBMODULE
	);
	const collapseBelow = $derived(Math.max(0, segs.length - 2));

	const commitDiff = resource(async () =>
		at ? getCommitDiff({ repo: repoKey, revision: at }) : undefined
	);

	const preview = resource(async () => {
		const [rev, file, sel, p, dir, cols] = [
			revision,
			isFile,
			selected,
			path,
			segs.join('/'),
			columns.data
		];
		if (!rev || empty) return { kind: 'none' as const };
		const commit = atCommit;
		const changes = commitDiff.data?.changes ?? [];
		if (sel?.kind === EntryKind.SUBMODULE) return { kind: 'submodule' as const, id: sel.id };
		if (sel?.kind === EntryKind.LINK) {
			const target = new TextDecoder().decode(
				(await getBlob({ repo: repoKey, revision: rev, path: p })).content
			);
			return { kind: 'link' as const, target, to: linkTarget(p, target) };
		}
		if (file && sel) {
			// Above the display cap the content is not fetched; GetBlob itself refuses 1 MiB.
			if (sel.size !== undefined && sel.size > DISPLAY_CAP)
				return { kind: 'large' as const, size: sel.size };
			let now: Uint8Array;
			try {
				now = (await getBlob({ repo: repoKey, revision: rev, path: p })).content;
			} catch (e) {
				if ((e as { code?: number }).code === Code.ResourceExhausted)
					return { kind: 'large' as const, size: sel.size };
				throw e;
			}
			let change = changes.find((c) => c.path === p);
			let shown = commit;
			// Not time-travelling: diff against the revision before the latest change of the file.
			if (!at) {
				shown = (await listCommits({ repo: repoKey, revision: rev, path: p, want: 1 })).commits[0];
				if (shown) change = (await getCommitDiff({ repo: repoKey, revision: shown.id })).changes.find((c) => c.path === p);
			}
			let before: Uint8Array | undefined;
			if (change?.kind === ChangeKind.ADDED) before = new Uint8Array();
			else if (shown && change && shown.parentIds[0])
				before = (
					await getBlob({ repo: repoKey, revision: shown.parentIds[0], path: p }).catch(
						() => undefined
					)
				)?.content;
			return { kind: 'file' as const, now, before, change, commit: shown };
		}
		if (at && p && changes.some((c) => c.path === p && c.kind === ChangeKind.DELETED))
			return { kind: 'deleted' as const };
		if (commit) return { kind: 'none' as const };
		const col = cols?.find((c) => c.dir === dir);
		const entry = README_NAMES.map((n) => col?.entries.find((e) => e.name === n)).find(Boolean);
		const readme = entry
			? new TextDecoder().decode(
					(
						await getBlob({
							repo: repoKey,
							revision: rev,
							path: dir ? `${dir}/${entry.name}` : entry.name
						})
					).content
				)
			: undefined;
		return { kind: 'readme' as const, readme, name: entry?.name };
	});
	const DISPLAY_CAP = 512 * 1024;
	/** The server-side raw download of the selected file (0021). */
	const rawHref = $derived(
		`/raw/${repoKey}/${encodeURI(at || q.get('ref') || refShortName(repo?.defaultBranch ?? ''))}/${path
			.split('/')
			.map(encodeURIComponent)
			.join('/')}`
	);
	/** A symlink target resolved against the link's directory; undefined if it leaves the repository. */
	function linkTarget(from: string, target: string) {
		const out = from.split('/').slice(0, -1);
		for (const part of target.split('/')) {
			if (part === '..') {
				if (!out.length) return undefined;
				out.pop();
			} else if (part && part !== '.') out.push(part);
		}
		return target.startsWith('/') ? undefined : out.join('/');
	}
	const fileMode = $derived(fileViewMode(q));
	const touched = $derived(touchedPaths(at ? commitDiff.data?.changes ?? [] : []));

	let scroller = $state<HTMLElement>();
	$effect(() => {
		void columns.data;
		tick().then(() => scroller?.scrollTo({ left: scroller.scrollWidth, behavior: 'smooth' }));
	});
	function onKey(e: KeyboardEvent, colIndex: number, keys: string[], cur?: string) {
		const i = cur ? keys.indexOf(cur) : -1;
		const base = segs.slice(0, colIndex).join('/');
		if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
			e.preventDefault();
			const n = keys[Math.max(0, Math.min(keys.length - 1, i + (e.key === 'ArrowDown' ? 1 : -1)))];
			if (n) go({ path: base ? `${base}/${n}` : n }).then(() => focusCol(colIndex));
		} else if (e.key === 'ArrowRight' || e.key === 'ArrowLeft') {
			e.preventDefault();
			focusCol(colIndex + (e.key === 'ArrowRight' ? 1 : -1));
		}
	}
	async function focusCol(i: number) {
		await tick();
		scroller?.querySelector<HTMLElement>(`[data-col="${i}"] a[tabindex="0"]`)?.focus();
	}

	// ── formatting ─────────────────────────────────────────────────────
	const fullDay = new Intl.DateTimeFormat('en', {
		weekday: 'short',
		month: 'short',
		day: 'numeric'
	});
	const hm = new Intl.DateTimeFormat('en', { hour: '2-digit', minute: '2-digit', hour12: false });
	const dur = (s: number) =>
		s >= 60 ? `${Math.floor(s / 60)}m${String(s % 60).padStart(2, '0')}s` : `${s}s`;
	const CHANGE: Record<ChangeKind, { l: string; cls: string; label: string }> = {
		[ChangeKind.UNSPECIFIED]: { l: '?', cls: 'text-fg-subtle', label: 'changed' },
		[ChangeKind.ADDED]: { l: 'A', cls: 'text-ok', label: 'added' },
		[ChangeKind.MODIFIED]: { l: 'M', cls: 'text-accent', label: 'modified' },
		[ChangeKind.DELETED]: { l: 'D', cls: 'text-danger', label: 'deleted' }
	};
	// History: events of the current scope (path-filtered log, its releases,
	// failed runs on those commits, a failing mirror)
	const events = $derived(
		buildEvents([{ repo: repoKey, commits: log.data ?? [], tags: tagsAt, runs: runs.data ?? [] }], {
			hideFixed: preferences.hideFixedFailures,
			mirrors: mirrors.data ?? [],
			runsOnListedCommits: true
		})
	);
	const counts = $derived(countEvents(events));
	const mirror = $derived.by(() => {
		const m = mirrors.data?.[0];
		if (!m) return undefined;
		if (m.state === MirrorState.OK)
			return {
				ok: true,
				t: m.lastSuccessTime
					? `mirror ok · ${relativeTime(toDate(m.lastSuccessTime))}`
					: 'mirror never pushed'
			};
		if (m.state === MirrorState.FAILING)
			return { ok: false, t: `mirror failing · ${m.attempts} attempts` };
		return { ok: true, t: 'mirror off' };
	});
	const scopeLabel = $derived(historyPath || repo?.name || '');
	const branchesAndReleases = $derived(
		[...(refs.data ?? [])].sort(
			(a, b) => Number(a.name.startsWith('refs/tags/')) - Number(b.name.startsWith('refs/tags/'))
		)
	);
	const stepClass = (s: RunState) =>
		s === RunState.SUCCEEDED
			? 'bg-ok'
			: s === RunState.FAILED
				? 'bg-danger'
				: s === RunState.RUNNING
					? 'bg-accent motion-safe:animate-pulse'
					: 'bg-border';
</script>

<!-- ── shared snippets ── -->
{#snippet strip(
	label: string,
	open: boolean,
	param: string,
	openValue: string | null,
	closedValue: string | null,
	summary: Snippet
)}
	<BandStrip {label} {open} href={hrefFor({ [param]: open ? closedValue : openValue })}>
		{@render summary()}
	</BandStrip>
{/snippet}

{#snippet runChain(list: Run[], n: number)}
	<span class="flex items-center gap-0.5" aria-label="Last {n} runs">
		{#each list.slice(0, n).toReversed() as r (r.id)}<RunBadge state={r.state} size={10} />{/each}
	</span>
{/snippet}

{#snippet stepsBar(r: Run, labels: boolean)}
	<div class="flex gap-0.5" aria-label="Steps">
		{#each r.steps as s, si (si)}
			<div
				class="min-w-0 flex-1"
				title="{s.name}: {RUN_LABEL[s.state]}{s.durationSeconds
					? ` · ${dur(s.durationSeconds)}`
					: ''}"
			>
				<div class="h-1 rounded-full {stepClass(s.state)}"></div>
				{#if labels}<div class="mt-1 truncate text-[0.6875rem] text-fg-subtle">{s.name}</div>{/if}
			</div>
		{/each}
	</div>
{/snippet}

{#snippet currentRun(r: Run, compact: boolean)}
	<div class="min-w-0">
		<a
			href="/-/runs/{r.id}"
			class="block min-w-0 rounded-md border border-border bg-bg px-3 py-2 hover:border-border-strong"
		>
			<div class="flex flex-wrap items-center gap-2 text-[0.8125rem]">
				<RunBadge state={r.state} label />
				<span title={r.id} class="min-w-0 truncate font-mono text-xs text-fg-muted">#{r.short}</span>
				<span class="ml-auto"><DurationBar seconds={r.durationSeconds} max={durationMax} /></span>
			</div>
			<p
				title="{refShortName(r.ref)} · {r.commitId} · {r.runner}"
				class="mt-1 truncate text-xs text-fg-muted"
			>
				{refShortName(r.ref)} · <span class="font-mono">{shortId(r.commitId)}</span> · {r.runner}
			</p>
			<div class="mt-2">{@render stepsBar(r, !compact)}</div>
		</a>
		{#key r.id}
			<RunActions run={r} allowed={canManageRuns(repo, viewer.data)} rerun={false} onchange={runs.retry} />
		{/key}
	</div>
{/snippet}

<div class="flex min-h-0 flex-1 flex-col lg:flex-row">
	<div class="relative flex min-h-0 min-w-0 flex-1 flex-col overflow-y-auto">
		<!-- ═══ Pulse ═══ -->
		{#snippet pulseSummary()}
			<span class="font-medium text-fg">{repo?.owner} / {repo?.name}</span>
			<span>last change {relativeTime(toDate(log.data?.[0]?.commitTime))}</span>
			{#if current && ciOn}<span class="flex items-center gap-1"
					><RunBadge state={current.state} size={10} />#{current.short}</span
				>{/if}
			{#if mirror}<span class={mirror.ok ? '' : 'text-danger'}>{mirror.t}</span>{/if}
		{/snippet}
		{@render strip('Pulse', pulseOpen, 'pulse', null, '0', pulseSummary)}
		{#if pulseOpen}
			<section class="shrink-0 bg-surface" aria-label="Pulse">
				<div
					class="grid grid-cols-[minmax(0,1fr)] gap-x-8 px-4 pt-4 pb-4 sm:px-6 xl:grid-cols-[minmax(0,1fr)_15rem]"
				>
					<div class="min-w-0">
						<div class="flex flex-wrap items-start gap-x-6 gap-y-2">
							<div class="min-w-0 flex-1 basis-full sm:basis-0">
								<h1 class="mt-1 text-2xl tracking-tight">
									<span class="text-fg-muted">{repo?.owner} /</span>
									<span class="font-semibold">{repo?.name}</span>
									{#if repo?.archived}<span class="ml-2 align-middle text-xs text-warn"
											>archived</span
										>{/if}
								</h1>
								{#if repo?.description}<p class="text-[0.8125rem] text-fg-muted">
										{repo.description}
									</p>{/if}
							</div>
							<div class="flex items-center gap-2 pt-4">
								{#if refs.data?.length && repo}
									<RefSelect
										refs={refs.data}
										value={head}
										onchange={(r) =>
											go({ ref: r === repo.defaultBranch ? null : refShortName(r), at: null })}
										class="h-8 w-36 rounded-sm border border-border"
									/>
								{/if}
								{#if repo?.cloneUrls && !empty}<CloneButton urls={repo.cloneUrls} />{/if}
							</div>
						</div>

						<!-- state line: administrative facts (+ CI in the inline variant) -->
						{#if repo}
							<p class="mt-2 flex flex-wrap items-center gap-x-4 gap-y-1 text-xs text-fg-subtle">
								<span class="flex items-center gap-1.5"
									><span
										class="size-1.5 rounded-full {ciOn
											? 'bg-ok'
											: 'border border-fg-subtle'}"
									></span>CI {ciOn ? 'on' : 'off'}</span
								>
								{#if mirror}<span class="flex items-center gap-1.5 {mirror.ok ? '' : 'text-danger'}"
										><Icon name="mirror" size={11} />{mirror.t}</span
									>{/if}
								<span
									>{repo.permissions
										.map((p) => `${p.user} ${ROLE_LABEL[p.role]}`)
										.join(' · ')}</span
								>
								<span class="flex items-center gap-1"><Icon name="lock" size={10} />declared</span>
							</p>
						{/if}

						{#if !empty}
							<figure class="mt-4">
								<figcaption class="mb-1.5 flex items-baseline gap-2 text-xs text-fg-subtle">
									<span>Activity of</span><span class="font-mono text-fg-muted">{scopeLabel}</span>
									<span class="tabular ml-auto">{inWindow} commits · 90 days</span>
								</figcaption>
								<div class="flex h-11 items-end gap-[2px]">
									{#each ribbon as day (day.k)}
										{@const n = day.commits.length}
										<button
											type="button"
											disabled={!n}
											onclick={() => go({ at: day.commits[0].id, day: day.k })}
											title="{fullDay.format(day.dt)} · {n} commit{n === 1 ? '' : 's'}{day.tags
												.length
												? ` · ${day.tags.join(', ')}`
												: ''}{day.ciState !== undefined ? ` · CI ${RUN_LABEL[day.ciState]}` : ''}"
											aria-label="{fullDay.format(day.dt)}, {n} commits"
											class="relative flex h-full flex-1 items-end enabled:cursor-pointer"
										>
											{#if day.tags.length}<span
													class="absolute -top-1.5 left-1/2 size-1.5 -translate-x-1/2 rotate-45 bg-warn"
													aria-hidden="true"
												></span>{/if}
											<span
												class="block w-full rounded-t-[2px] {day.k === atDay
													? 'bg-fg'
													: n
														? 'bg-accent/75 hover:bg-accent'
														: 'bg-border/70'}"
												style:height="{n ? Math.max(14, (n / ribbonMax) * 100) : 5}%"
											></span>
										</button>
									{/each}
								</div>
							</figure>

							<ul
								aria-label="Pulse commits"
								class="mt-3 max-h-48 overflow-y-auto overscroll-contain space-y-0.5 text-[0.8125rem]"
							>
								{#each recent as c (c.id)}
									{@const run = runByCommit.get(c.id)}
									<li>
										<a
											href={hrefFor({ at: c.id === at ? null : c.id })}
											data-sveltekit-noscroll
											class="flex items-center gap-3 rounded-sm px-1 py-0.5 hover:bg-surface-2 {c.id ===
											at
												? 'bg-surface-2'
												: ''}"
										>
											<span class="w-3"
												>{#if run}<RunBadge state={run.state} size={11} />{/if}</span
											>
											<span
												class="w-14 shrink-0 font-mono text-xs {c.id === at
													? 'text-fg'
													: 'text-link'}">{shortId(c.id)}</span
											>
											<span class="min-w-0 flex-1 truncate {c.id === at ? 'font-medium' : ''}"
												>{commitSubject(c.message)}</span
											>
											<span class="hidden shrink-0 text-xs text-fg-subtle sm:inline"
												>{c.authorName} · {relativeTime(toDate(c.commitTime))}</span
											>
										</a>
									</li>
								{:else}
									<li class="text-xs text-fg-subtle">
										No commit touched {scopeLabel} on this branch.
									</li>
								{/each}
							</ul>
						{/if}
					</div>

					<!-- branches & releases (the "threads") -->
					{#if refs.data?.length}
						<aside
							class="mt-5 border-t border-border pt-4 xl:mt-0 xl:border-t-0 xl:border-l xl:pt-0 xl:pl-6"
							aria-label="Branches and releases"
						>
							<h2 class="mb-2 text-xs tracking-wide text-fg-subtle uppercase">
								Branches & releases
							</h2>
							<ul class="space-y-0.5 text-[0.8125rem]">
								{#each branchesAndReleases as r (r.name)}
									{@const isTag = r.name.startsWith('refs/tags/')}
									{@const active = isTag ? at === r.commitId : r.name === head && !at}
									<li>
										<a
											href={isTag
												? hrefFor({ at: r.commitId })
												: hrefFor({
														ref: r.name === repo?.defaultBranch ? null : refShortName(r.name),
														at: null
													})}
											data-sveltekit-noscroll
											class="flex items-center gap-2 rounded-sm px-1 py-0.5 hover:bg-surface-2 {active
												? 'bg-surface-2 text-fg'
												: 'text-fg-muted'}"
										>
											<span
												class="size-1.5 shrink-0 {isTag
													? 'rotate-45 bg-warn'
													: 'rounded-full bg-accent'}"
												aria-hidden="true"
											></span>
											<span class="truncate font-mono text-xs">{refShortName(r.name)}</span>
											<span class="ml-auto shrink-0 text-[0.6875rem] text-fg-subtle"
												>{isTag
													? 'release'
													: r.name === repo?.defaultBranch
														? 'default'
														: 'branch'}</span
											>
										</a>
									</li>
								{/each}
							</ul>
						</aside>
					{/if}
				</div>
			</section>
		{/if}

		<!-- ═══ Pipelines (band variant) ═══ -->
		{#if ciOn && runs.data?.length}
			{#snippet pipeSummary()}
				{#if current}<span class="flex items-center gap-1.5"
						><RunBadge state={current.state} size={10} /><span class="text-fg-muted"
							>#{current.short} {RUN_LABEL[current.state]}</span
						>
						· {dur(current.durationSeconds)}</span
					>{/if}
				{@render runChain(branchRuns, 12)}
			{/snippet}
			{@render strip('Pipelines', pipelinesOpen, 'pipelines', null, '0', pipeSummary)}
			{#if pipelinesOpen}
				<section
					class="shrink-0 border-b border-border bg-surface px-4 py-3 sm:px-6"
					aria-label="Pipelines"
				>
					<div class="grid grid-cols-[minmax(0,1fr)] gap-4 lg:grid-cols-[minmax(0,20rem)_minmax(0,1fr)]">
						{#if current}{@render currentRun(current, false)}{/if}
						<div class="min-w-0">
							<ul class="relative flex gap-2 overflow-x-auto pb-1">
								{#each branchRuns.filter((r) => r.id !== current?.id).slice(0, 8) as r (r.id)}
									<li class="shrink-0">
										<a
											href="/-/runs/{r.id}"
											class="block w-36 rounded-md border border-border px-2.5 py-1.5 text-xs hover:border-border-strong"
										>
											<span class="flex flex-wrap items-center gap-1.5"
												><RunBadge state={r.state} size={11} /><span title={r.id} class="min-w-0 truncate font-mono text-fg-muted"
													>#{r.short}</span
												><span class="tabular ml-auto text-fg-subtle">{dur(r.durationSeconds)}</span
												></span
											>
											<span
												title="{relativeTime(toDate(r.createTime))} · {r.commitId}"
												class="mt-0.5 block truncate text-fg-subtle"
												>{relativeTime(toDate(r.createTime))} ·
												<span class="font-mono">{shortId(r.commitId)}</span></span
											>
										</a>
										<RunActions run={r} allowed={canManageRuns(repo, viewer.data)} rerun={false} onchange={runs.retry} />
									</li>
								{/each}
							</ul>
						</div>
					</div>
					{#if worktreeRuns.length}
						<h2 class="mt-3 text-xs text-fg-subtle">Worktree runs</h2>
						<!-- svelte-ignore a11y_no_noninteractive_tabindex (scrollable run list) -->
						<ul class="mt-1 max-h-64 overflow-auto" tabindex="0" aria-label="Worktree runs">
							{#each worktreeRuns as r (r.id)}
								<li class="flex min-w-[42rem] items-center gap-2 border-b border-border/50 py-1.5">
									<a href="/-/runs/{r.id}" class="flex min-w-0 flex-1 items-center gap-3 text-xs hover:bg-surface-2" title={runLabel(r)}>
										<span class="rounded-sm border border-accent/40 bg-accent-soft px-1 text-accent">worktree</span>
										<span class="w-20 shrink-0 truncate">{r.triggeredBy || 'unknown user'}</span>
										<span class="shrink-0 font-mono">{r.baseCommitId ? shortId(r.baseCommitId) : 'unknown base'}</span>
										<span class="min-w-20 flex-1 truncate">{r.checks.length ? r.checks.join(', ') : 'all checks'}</span>
										<RunBadge state={r.state} size={11} />
										<span class="tabular shrink-0">{dur(r.durationSeconds)}</span>
										<span class="shrink-0 text-fg-subtle">{relativeTime(toDate(r.createTime))}</span>
									</a>
									<RunActions run={r} allowed={canManageRuns(repo, viewer.data)} rerun={false} onchange={runs.retry} />
								</li>
							{/each}
						</ul>
					{/if}
				</section>
			{/if}
		{/if}

		<!-- ═══ History ═══ -->
		{#if !empty}
			{#snippet historySummary()}
				<span class="tabular"
					>{counts.commits}{logMore ? '+' : ''} commits touch
					<span class="font-mono text-fg-muted">{scopeLabel}</span></span
				>
				{#if counts.release}<span class="tabular text-warn"
						>{counts.release} release{counts.release > 1 ? 's' : ''}</span
					>{/if}
				{#if counts.ci}<span class="tabular"
						>{counts.ci} CI run{counts.ci > 1 ? 's' : ''}</span
					>{/if}
				{#if counts.mirror}<span class="text-danger">mirror failing</span>{/if}
			{/snippet}
			{#if path}
				<a href={hrefFor({ path: null, scope: '', history: '1' })} data-sveltekit-noscroll class="block px-6 py-2 text-xs text-link hover:underline">Repository history · {refShortName(head)}</a>
			{/if}
			{@render strip('History', historyOpen, 'history', '1', null, historySummary)}
			{#if historyOpen}
				<section
					class="relative max-h-[45vh] shrink-0 overflow-y-auto border-b border-border bg-surface"
					aria-label="History"
				>
					<HistoryFeed
						{events}
						{runByCommit}
						commitHref={(_, id) => hrefFor({ at: id })}
						current={at}
						groupLimit={8}
					/>
				</section>
			{/if}
		{/if}

		{#if at && atCommit}
			<div
				class="flex shrink-0 items-center gap-3 border-b border-border bg-accent-soft px-4 py-1.5 text-xs sm:px-6"
				role="status"
			>
				<Icon name="clock" size={13} class="text-accent" />
				<span class="text-accent"
					>As of <span class="font-mono font-medium">{shortId(at)}</span></span
				>
				<span class="hidden truncate text-fg-muted sm:inline"
					>{commitSubject(atCommit.message)} · {atCommit.authorName} · {relativeTime(
						toDate(atCommit.commitTime)
					)}</span
				>
				{#if runByCommit.get(at)}{@const r = runByCommit.get(at)!}<a
						href="/-/runs/{r.id}"
						class="flex items-center gap-1 text-fg-muted hover:underline"
						><RunBadge state={r.state} size={11} />CI #{r.short}</a
					>{/if}
				<a
					href={hrefFor({ at: null })}
					data-sveltekit-noscroll
					class="ml-auto text-accent hover:underline">Back to now</a
				>
			</div>
		{/if}

		{#if at && atCommit}
			{@const c = atCommit}
			<section aria-label="Selected commit" class="shrink-0 border-b-2 border-accent bg-accent-soft px-6 py-4">
				<div class="flex flex-wrap items-center gap-2 text-xs">
					<span class="tracking-wide text-accent uppercase">Selected commit</span>
					<code class="break-all">{c.id}</code><CopyButton text={c.id} label="Copy commit id" />
				</div>
				<h2 class="mt-2 text-lg font-semibold">{commitSubject(c.message)}</h2>
				{#if commitBody(c.message)}<p class="mt-2 text-sm whitespace-pre-wrap">{commitBody(c.message)}</p>{/if}
				<p class="mt-2 text-xs text-fg-muted">Author: {c.authorName} &lt;{c.authorEmail}&gt;</p>
				<p class="mt-1 text-xs text-fg-muted">Committer: {c.committerName} &lt;{c.committerEmail}&gt;</p>
				<time class="mt-1 block text-xs text-fg-muted" datetime={toDate(c.commitTime)?.toISOString()} title={toDate(c.commitTime)?.toUTCString()}>{localDateTime(toDate(c.commitTime))}</time>
				{#if runByCommit.get(c.id)}
					<div class="mt-4 max-w-sm">{@render currentRun(runByCommit.get(c.id)!, false)}</div>
				{/if}
				<!-- TODO: show verified signature status when the browsing API exposes it; push admission alone cannot verify imported history. -->
				<h3 class="mt-4 text-xs tracking-wide text-fg-subtle uppercase">{commitDiff.data?.changes.length ?? 0} files changed</h3>
				{#if commitDiff.error}<p role="alert" class="text-sm text-danger">Could not load the changed files.</p>{/if}
				<ul aria-label="Files touched by selected commit" class="mt-2 max-h-48 overflow-y-auto">
					{#each commitDiff.data?.changes ?? [] as ch (`${ch.kind}:${ch.path}`)}
						{@const k = CHANGE[ch.kind]}
						<li><a href={hrefFor({ path: ch.path, view: 'diff' })} data-sveltekit-noscroll aria-current={path === ch.path ? 'true' : undefined} class="flex gap-3 py-1 font-mono text-xs hover:underline">
							<span class={k.cls} title={k.label}>{k.l}</span><span class="break-all">{ch.path}</span>
						</a></li>
					{/each}
				</ul>
			</section>
		{/if}

		<!-- ═══ Files ═══ -->
		{#if !empty}
			{#snippet filesSummary()}
				<span
					>{columns.data?.[0]?.entries.length ?? 0} entries at
					<span class="font-mono">{at ? shortId(at) : refShortName(head)}</span></span
				>
				{#if path}<span class="font-mono text-fg-muted">{path}</span>{/if}
			{/snippet}
			{@render strip('Files', filesOpen, 'files', '1', '0', filesSummary)}
		{/if}

		<!-- ═══ Workspace ═══ -->
		<div bind:this={scroller} class="flex min-h-[56rem] shrink-0 overflow-x-auto bg-surface" data-workspace>
			{#if empty}
				<section class="max-w-2xl px-4 py-8 text-[0.8125rem] sm:px-6" aria-label="Empty repository">
					<p class="text-xs tracking-wide text-fg-subtle uppercase">Nothing pushed yet</p>
					<p class="mt-1 text-fg-muted">
						The repository is declared but has no commits. Push an existing one (commits must be
						SSH-signed):
					</p>
					<pre
						class="mt-3 rounded-sm border border-border bg-surface px-3 py-2 font-mono text-xs leading-5 break-all whitespace-pre-wrap text-fg-muted">git remote add origin {repo?.cloneUrls?.ssh ?? ''}
git push -u origin {refShortName(repo?.defaultBranch ?? 'refs/heads/main')}</pre>
					<p class="mt-3 text-xs text-fg-subtle">
						History, pipelines and files appear here after the first push.
					</p>
				</section>
			{:else}
				{#if filesOpen}
					{#each columns.data ?? [] as col, i (col.dir)}
						{@const sel = segs[i]}
						{#if i < collapseBelow && sel}
							<a
								href={hrefFor({ path: segs.slice(0, i + 1).join('/') })}
								data-sveltekit-noscroll
								title="Back to {sel}"
								data-touched={touched.has(segs.slice(0, i + 1).join('/')) ? 'true' : undefined}
								class:bg-accent-soft={touched.has(segs.slice(0, i + 1).join('/'))}
								class="hidden w-7 shrink-0 justify-center border-r border-border pt-3 text-[0.6875rem] text-fg-subtle hover:bg-surface hover:text-fg md:flex"
							>
								<span class="font-mono [writing-mode:vertical-rl]">{sel}</span>
							</a>
						{:else}
							<section
								class="w-56 shrink-0 flex-col border-r border-border max-md:w-full {(isFile
									? -1
									: (columns.data?.length ?? 1) - 1) === i
									? 'flex'
									: 'hidden md:flex'}"
								aria-label={col.dir || 'root'}
							>
								<h2 class="truncate px-4 pt-3 pb-1 font-mono text-[0.6875rem] text-fg-subtle">
									{col.dir ? col.dir.split('/').pop() + '/' : '/'}
								</h2>
								<ul class="flex-1 overflow-y-auto pb-2" data-col={i}>
									{#each col.entries as e, n (e.name)}
										{@const isSel = e.name === sel}
										{@const changed = touched.has(col.dir ? `${col.dir}/${e.name}` : e.name)}
										<li>
											<a
												href={hrefFor({ path: col.dir ? `${col.dir}/${e.name}` : e.name })}
												data-sveltekit-noscroll
												data-sveltekit-keepfocus
												tabindex={isSel || (!sel && n === 0) ? 0 : -1}
												aria-current={isSel ? 'true' : undefined}
												data-touched={changed ? 'true' : undefined}
												title={changed ? (e.kind === EntryKind.TREE ? 'Contains changes in selected commit' : 'Changed in selected commit') : undefined}
												onkeydown={(ev) =>
													onKey(
														ev,
														i,
														col.entries.map((x) => x.name),
														sel
													)}
												class="{changed ? (e.kind === EntryKind.TREE ? 'bg-accent-soft/40' : 'bg-accent-soft') : ''} relative flex items-center gap-2 py-[3px] pr-3 pl-4 text-[0.8125rem] {isSel
													? 'font-medium text-fg'
													: 'text-fg-muted hover:text-fg'}"
											>
												{#if isSel}<span
														class="absolute inset-y-0.5 left-0 w-0.5 rounded-r bg-accent"
														aria-hidden="true"
													></span>{/if}
												<Icon
													name={ENTRY_ICON[e.kind] as IconName}
													size={13}
													class={e.kind === EntryKind.TREE ? 'text-accent/80' : 'text-fg-subtle'}
												/>
												<span class="min-w-0 flex-1 truncate">{e.name}</span>
												{#if e.size !== undefined}<span class="tabular text-xs text-fg-subtle"
														>{formatBytes(e.size)}</span
													>{/if}
											</a>
										</li>
									{/each}
								</ul>
							</section>
						{/if}
					{/each}
				{/if}

				<!-- svelte-ignore a11y_no_noninteractive_tabindex (scrollable: must be focusable) -->
				<section
					tabindex="0"
					class="min-w-0 flex-1 overflow-y-auto bg-surface {filesOpen
						? 'md:min-w-[30rem]'
						: ''} {filesOpen && !isFile ? 'max-md:hidden' : ''}"
					aria-label="Preview"
				>
					{#if preview.data?.kind === 'readme'}
						{#if preview.data.readme}
							<div class="mx-auto max-w-3xl px-6 py-8 lg:px-10">
								<p class="mb-4 font-mono text-xs text-fg-subtle">
									{segs.length ? segs.join('/') + '/' : ''}{preview.data.name}
								</p>
								<Markdown source={preview.data.readme} />
							</div>
						{:else}
							<p class="p-8 text-[0.8125rem] text-fg-subtle">
								No README here. Open Files, or pick a day in the activity ribbon.
							</p>
						{/if}
					{:else if preview.data?.kind === 'submodule'}
						<div class="p-6 text-[0.8125rem] text-fg-muted">
							<p class="font-mono text-xs text-fg-subtle">{path}</p>
							<p class="mt-2">Submodule at commit <span class="font-mono">{preview.data.id}</span></p>
							<p class="mt-1 text-xs text-fg-subtle">Its content lives in another repository.</p>
						</div>
					{:else if preview.data?.kind === 'link'}
						<div class="p-6 text-[0.8125rem] text-fg-muted">
							<p class="font-mono text-xs text-fg-subtle">{path}</p>
							<p class="mt-2">
								Symbolic link to
								{#if preview.data.to !== undefined}
									<a
										href={hrefFor({ path: preview.data.to })}
										data-sveltekit-noscroll
										class="font-mono text-accent hover:underline">{preview.data.target}</a
									>
								{:else}<span class="font-mono">{preview.data.target}</span>{/if}
							</p>
						</div>
					{:else if preview.data?.kind === 'large'}
						<div class="p-6 text-[0.8125rem] text-fg-muted">
							<p class="font-mono text-xs text-fg-subtle">{path}</p>
							<p class="mt-2">
								Large file{preview.data.size !== undefined
									? ` (${formatBytes(preview.data.size)})`
									: ''}: not previewed.
								<a href={rawHref} class="text-accent hover:underline">Download the raw file</a>
							</p>
						</div>
					{:else if preview.data?.kind === 'deleted'}
						<p class="p-6 text-sm text-fg-muted">{path} was deleted in this commit.</p>
					{:else if preview.data?.kind === 'file'}
						{@const f = preview.data}
						{@const text = new TextDecoder().decode(f.now)}
						{@const hunks =
							f.before && !isBinary(f.now)
								? lineDiff(new TextDecoder().decode(f.before), text)
								: undefined}
						<div class="flex min-w-0 flex-col">
							<div
								class="flex flex-wrap items-center gap-x-3 gap-y-1 border-b border-border px-4 py-2 text-xs text-fg-subtle"
							>
								<span class="font-mono text-[0.8125rem] font-medium text-fg">{path}</span>
								<span
									>{formatBytes(f.now.length)}{languageFor(path)
										? ` · ${languageFor(path)}`
										: ''}</span
								>
								<a href={rawHref} class="hover:text-fg hover:underline">Raw</a>
								{#if f.commit}<span class={f.change ? CHANGE[f.change.kind].cls : ''}
										>{f.change ? CHANGE[f.change.kind].label : 'unchanged'} in {shortId(
											f.commit.id
										)}</span
									>{/if}
								{#if hunks}
									<div
										class="ml-auto flex rounded-sm border border-border p-0.5"
										role="group"
										aria-label="View"
									>
										{#each ['diff', 'file'] as const as m (m)}
											<button
												type="button"
												aria-pressed={fileMode === m}
												onclick={() => go({ view: m === 'diff' ? 'diff' : null })}
												class="rounded-[3px] px-2 py-0.5 {fileMode === m
													? 'bg-accent-soft text-accent'
													: 'hover:text-fg'}">{m === 'diff' ? 'Diff' : 'File'}</button
											>
										{/each}
									</div>
								{/if}
							</div>
							<!-- svelte-ignore a11y_no_noninteractive_tabindex (scrollable: must be focusable) -->
							<div tabindex="0" class="min-h-0 flex-1 overflow-auto">
								{#if isBinary(f.now)}
									<p class="p-4 text-[0.8125rem] text-fg-muted">
										Binary file, {formatBytes(f.now.length)}.
									</p>
								{:else if hunks && fileMode === 'diff'}
									<DiffView {hunks} language={languageFor(path)} />
								{:else if isMarkdown(path)}
									<div class="mx-auto max-w-3xl px-6 py-8"><Markdown source={text} /></div>
								{:else}
									<CodeView code={text} language={languageFor(path)} />
								{/if}
							</div>
						</div>
					{/if}
				</section>
			{/if}
		</div>
	</div>
</div>
