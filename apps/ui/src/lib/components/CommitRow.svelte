<!-- One commit in a history list: subject, expandable body, author, time, id. -->
<script lang="ts">
	import type { Commit, Repo } from '#lib/api.ts';
	import {
		absoluteTime,
		commitBody,
		commitSubject,
		relativeTime,
		shortId,
		toDate
	} from '#lib/format.ts';
	import { commitHref } from '#lib/repo-ui.ts';
	import CopyButton from './CopyButton.svelte';

	let { repo, commit }: { repo: Repo; commit: Commit } = $props();

	const when = $derived(toDate(commit.commitTime));
	const body = $derived(commitBody(commit.message));
	let open = $state(false);
</script>

<li class="flex gap-3 px-4 py-2.5">
	<div class="min-w-0 flex-1">
		<div class="flex items-baseline gap-2">
			<a
				href={commitHref(repo, commit.id)}
				class="min-w-0 truncate text-[0.8125rem] font-medium hover:text-link hover:underline"
				>{commitSubject(commit.message)}</a
			>
			{#if body}
				<button
					type="button"
					onclick={() => (open = !open)}
					aria-expanded={open}
					aria-label="{open ? 'Hide' : 'Show'} commit message body"
					class="shrink-0 rounded-sm bg-surface-2 px-1.5 text-xs leading-4 text-fg-muted hover:text-fg"
					>…</button
				>
			{/if}
			{#if commit.parentIds.length > 1}
				<span
					class="shrink-0 rounded-full border border-border px-1.5 text-[0.6875rem] text-fg-subtle"
					>merge</span
				>
			{/if}
		</div>
		{#if open}
			<pre class="mt-1.5 font-sans text-xs whitespace-pre-wrap text-fg-muted">{body}</pre>
		{/if}
		<p class="mt-0.5 text-xs text-fg-subtle">
			<span class="text-fg-muted">{commit.authorName}</span> ·
			<time datetime={when?.toISOString()} title={absoluteTime(when)}>{relativeTime(when)}</time>
		</p>
	</div>
	<div class="flex shrink-0 items-start gap-0.5">
		<a
			href={commitHref(repo, commit.id)}
			class="font-mono text-xs leading-6 text-link hover:underline">{shortId(commit.id)}</a
		>
		<CopyButton
			text={commit.id}
			label="Copy full commit id"
			class="p-1.5 text-fg-subtle hover:text-fg"
		/>
	</div>
</li>
