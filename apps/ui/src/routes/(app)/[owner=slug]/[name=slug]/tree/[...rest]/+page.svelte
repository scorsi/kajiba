<!--
	/<owner>/<name>/tree/<ref>/<path>: directory listing or file view.
	Left: the directory (for a file: its parent, file highlighted).
	Right: the directory README, or the file.
-->
<script lang="ts">
	import { page } from '$app/state';
	import { ApiError, Code, EntryKind, getBlob } from '#lib/api.ts';
	import Breadcrumbs from '#lib/components/Breadcrumbs.svelte';
	import FileList from '#lib/components/FileList.svelte';
	import FileView from '#lib/components/FileView.svelte';
	import Markdown from '#lib/components/Markdown.svelte';
	import Skeleton from '#lib/components/Skeleton.svelte';
	import StateView from '#lib/components/StateView.svelte';
	import { getRepoContext } from '#lib/context.ts';
	import { refShortName, shortId } from '#lib/format.ts';
	import { loadDirectory, type DirectoryView } from '#lib/loaders.ts';
	import { splitRefPath, treeHref } from '#lib/repo-ui.ts';
	import { resource } from '#lib/resource.svelte.ts';

	const ctx = getRepoContext();
	const target = $derived(splitRefPath(page.params.rest ?? '', ctx.refs));

	interface View {
		revision: string;
		path: string;
		kind: EntryKind;
		/** The directory shown on the left (for a file: its parent). */
		dir: DirectoryView;
		parentPath?: string;
		name?: string;
		blob?: { commitId: string; content: Uint8Array };
	}

	const view = resource<View>(async () => {
		const t = target;
		if (!t)
			throw new ApiError(Code.NotFound, `unknown branch, tag or commit in '${page.params.rest}'`);
		if (!t.path) {
			return { ...t, kind: EntryKind.TREE, dir: await loadDirectory(ctx.key, t.revision) };
		}
		const parentPath = t.path.split('/').slice(0, -1).join('/');
		const name = t.path.split('/').pop()!;
		const parent = await loadDirectory(ctx.key, t.revision, parentPath);
		const entry = parent.entries.find((e) => e.name === name);
		if (!entry) throw new ApiError(Code.NotFound, `no '${t.path}' at ${refShortName(t.revision)}`);
		if (entry.kind === EntryKind.TREE) {
			return { ...t, kind: entry.kind, dir: await loadDirectory(ctx.key, t.revision, t.path) };
		}
		const blob = await getBlob({ repo: ctx.key, revision: t.revision, path: t.path });
		return { ...t, kind: entry.kind, dir: parent, parentPath, name, blob };
	});

	const v = $derived(view.data);
	const isCommitId = $derived(!!v && /^[0-9a-f]{40}$/.test(v.revision));
</script>

<svelte:head>
	<title>{target?.path || ctx.repo.name} · {ctx.repo.owner}/{ctx.repo.name}</title>
</svelte:head>

{#if view.error && !view.loading}
	<StateView error={view.error} what="path" onretry={view.retry} />
{:else if !v}
	<div class="grid gap-5 xl:grid-cols-[22rem_minmax(0,1fr)]">
		<div class="rounded-md border border-border bg-surface p-3"><Skeleton lines={10} /></div>
		<div class="rounded-md border border-border bg-surface p-5"><Skeleton lines={14} /></div>
	</div>
{:else}
	<div class="mb-4 flex flex-wrap items-center gap-x-3 gap-y-1">
		<Breadcrumbs repo={ctx.repo} revision={v.revision} path={v.path} />
		{#if isCommitId}
			<span class="rounded-full bg-surface-2 px-2 py-0.5 text-xs text-fg-muted"
				>at commit <span class="font-mono">{shortId(v.revision)}</span></span
			>
		{/if}
	</div>

	{#if v.blob}
		<div class="grid gap-5 xl:grid-cols-[22rem_minmax(0,1fr)] {view.loading ? 'opacity-60' : ''}">
			<div class="order-last min-w-0 xl:order-none">
				<FileList
					repo={ctx.repo}
					refs={ctx.refs}
					revision={v.revision}
					path={v.parentPath ?? ''}
					entries={v.dir.entries}
					lastCommit={v.dir.lastCommit}
					activeName={v.name}
				/>
			</div>
			<FileView
				repo={ctx.repo}
				revision={v.revision}
				commitId={v.blob.commitId}
				path={v.path}
				kind={v.kind}
				content={v.blob.content}
			/>
		</div>
	{:else if v.dir.readme}
		<div class="grid gap-5 xl:grid-cols-[22rem_minmax(0,1fr)] {view.loading ? 'opacity-60' : ''}">
			<FileList
				repo={ctx.repo}
				refs={ctx.refs}
				revision={v.revision}
				path={v.path}
				entries={v.dir.entries}
				lastCommit={v.dir.lastCommit}
			/>
			<article class="min-w-0 rounded-md border border-border bg-surface">
				<h2 class="border-b border-border px-5 py-2.5 text-[0.8125rem] font-medium text-fg-muted">
					{v.dir.readme.name}
				</h2>
				<Markdown
					source={v.dir.readme.source}
					linkBase={treeHref(ctx.repo, v.revision, v.path) + '/'}
					class="p-5 sm:p-7"
				/>
			</article>
		</div>
	{:else}
		<div class="max-w-3xl {view.loading ? 'opacity-60' : ''}">
			<FileList
				repo={ctx.repo}
				refs={ctx.refs}
				revision={v.revision}
				path={v.path}
				entries={v.dir.entries}
				lastCommit={v.dir.lastCommit}
			/>
		</div>
	{/if}
{/if}
