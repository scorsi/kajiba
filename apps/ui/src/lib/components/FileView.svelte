<!--
	One file at a ref: text (highlighted), Markdown (rendered/source), image,
	other binary, large file, or symlink. Download works from the bytes we
	already have (no raw endpoint needed).
-->
<script lang="ts">
	import { EntryKind, type Repo } from '#lib/api.ts';
	import { formatBytes, isBinary, refShortName, shortId } from '#lib/format.ts';
	import { languageFor } from '#lib/highlight.ts';
	import { commitsHref, treeHref } from '#lib/repo-ui.ts';
	import CodeView from './CodeView.svelte';
	import CopyButton from './CopyButton.svelte';
	import Icon from './Icon.svelte';
	import Markdown from './Markdown.svelte';

	let {
		repo,
		revision,
		commitId,
		path,
		kind,
		content
	}: {
		repo: Repo;
		revision: string;
		commitId: string;
		path: string;
		kind: EntryKind;
		content: Uint8Array;
	} = $props();

	/** Above this size, show a notice instead of the content (until asked). */
	const DISPLAY_LIMIT = 512 * 1024;
	const IMAGE_TYPES: Record<string, string> = {
		png: 'image/png',
		jpg: 'image/jpeg',
		jpeg: 'image/jpeg',
		gif: 'image/gif',
		webp: 'image/webp',
		avif: 'image/avif',
		ico: 'image/x-icon',
		svg: 'image/svg+xml'
	};

	const name = $derived(path.split('/').pop() ?? path);
	const ext = $derived(name.includes('.') ? name.split('.').pop()!.toLowerCase() : '');
	const imageType = $derived(IMAGE_TYPES[ext]);
	const binary = $derived(kind !== EntryKind.SYMLINK && isBinary(content));
	const tooLarge = $derived(content.length > DISPLAY_LIMIT);
	const isMarkdown = $derived(ext === 'md' || ext === 'markdown');
	const language = $derived(languageFor(path));

	let force = $state(false);
	let mode = $state<'rendered' | 'source'>('rendered');

	const text = $derived(
		(!binary && (!tooLarge || force)) || kind === EntryKind.SYMLINK
			? new TextDecoder().decode(content)
			: ''
	);
	const lineCount = $derived(
		binary
			? 0
			: content.reduce((n, b) => (b === 10 ? n + 1 : n), 0) + (content.at(-1) === 10 ? 0 : 1)
	);

	/** Object URL for download / image preview, revoked when content changes. */
	let objectUrl = $state<string>();
	$effect(() => {
		const url = URL.createObjectURL(
			new Blob([content as BlobPart], { type: imageType ?? 'application/octet-stream' })
		);
		objectUrl = url;
		return () => URL.revokeObjectURL(url);
	});

	/** Symlink target resolved against the link's directory. */
	const linkTarget = $derived.by(() => {
		if (kind !== EntryKind.SYMLINK) return undefined;
		const parts = path.split('/').slice(0, -1);
		for (const seg of text.trim().split('/')) {
			if (seg === '..') parts.pop();
			else if (seg && seg !== '.') parts.push(seg);
		}
		return parts.join('/');
	});

	const meta = $derived(
		kind === EntryKind.SYMLINK
			? 'symbolic link'
			: [!binary && `${lineCount} lines`, formatBytes(content.length), !binary && language]
					.filter(Boolean)
					.join(' · ')
	);

	const btn =
		'inline-flex h-7 items-center gap-1.5 rounded-sm px-2 text-xs text-fg-muted hover:bg-surface-2 hover:text-fg';
</script>

<article class="min-w-0 rounded-md border border-border bg-surface" aria-label={name}>
	<header class="flex flex-wrap items-center gap-x-3 gap-y-1 border-b border-border px-3 py-2">
		<h2 class="flex min-w-0 items-center gap-2 font-mono text-[0.8125rem] font-medium">
			<Icon
				name={kind === EntryKind.SYMLINK ? 'symlink' : 'file'}
				size={15}
				class="text-fg-subtle"
			/>
			<span class="truncate">{name}</span>
		</h2>
		<span class="tabular text-xs text-fg-subtle">
			{meta}
		</span>

		<div class="ml-auto flex items-center gap-0.5">
			{#if isMarkdown && !tooLarge && kind !== EntryKind.SYMLINK}
				<div class="mr-1 flex rounded-sm border border-border p-0.5" role="group" aria-label="View">
					{#each ['rendered', 'source'] as const as m (m)}
						<button
							type="button"
							aria-pressed={mode === m}
							onclick={() => (mode = m)}
							class="rounded-[3px] px-2 py-0.5 text-xs {mode === m
								? 'bg-accent-soft text-accent'
								: 'text-fg-muted hover:text-fg'}">{m === 'rendered' ? 'Rendered' : 'Source'}</button
						>
					{/each}
				</div>
			{/if}
			{#if !binary && text}
				<CopyButton {text} label="Copy file contents" class={btn} />
			{/if}
			<a href={objectUrl} download={name} class={btn} title="Download {name}">Download</a>
			<a
				href={treeHref(repo, commitId, path)}
				class="{btn} hidden sm:inline-flex"
				title="Permalink at {shortId(commitId)}">Permalink</a
			>
			<a
				href={commitsHref(repo, revision)}
				class="{btn} hidden sm:inline-flex"
				title="History of {refShortName(revision)}">History</a
			>
		</div>
	</header>

	{#if kind === EntryKind.SYMLINK}
		<div class="px-4 py-6 text-[0.8125rem]">
			Symbolic link to
			<a href={treeHref(repo, revision, linkTarget)} class="font-mono text-link hover:underline"
				>{text.trim()}</a
			>
		</div>
	{:else if binary && imageType}
		<div
			class="flex justify-center bg-[repeating-conic-gradient(var(--k-surface-2)_0_25%,transparent_0_50%)] bg-[length:16px_16px] p-6"
		>
			<img src={objectUrl} alt={name} class="max-h-[70dvh] max-w-full [image-rendering:auto]" />
		</div>
	{:else if binary}
		<div class="px-4 py-10 text-center text-[0.8125rem] text-fg-muted">
			<Icon name="file" size={24} class="mx-auto text-fg-subtle" />
			<p class="mt-2">Binary file, {formatBytes(content.length)}. Not displayed.</p>
			<a href={objectUrl} download={name} class="mt-2 inline-block text-link hover:underline"
				>Download</a
			>
		</div>
	{:else if tooLarge && !force}
		<div class="px-4 py-10 text-center text-[0.8125rem] text-fg-muted">
			<p>
				Large file ({formatBytes(content.length)}, {lineCount} lines). Not displayed by default.
			</p>
			<p class="mt-2 flex justify-center gap-4">
				<a href={objectUrl} download={name} class="text-link hover:underline">Download</a>
				<button type="button" onclick={() => (force = true)} class="text-link hover:underline"
					>Show anyway (no highlighting)</button
				>
			</p>
		</div>
	{:else if isMarkdown && mode === 'rendered'}
		<Markdown
			source={text}
			linkBase={treeHref(repo, revision, path.split('/').slice(0, -1).join('/')) + '/'}
			class="p-5 sm:p-7"
		/>
	{:else}
		<CodeView code={text} language={force ? undefined : language} />
	{/if}
</article>
