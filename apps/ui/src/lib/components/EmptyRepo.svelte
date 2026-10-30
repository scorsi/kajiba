<!-- Repository with no refs yet: how to push the first commit. -->
<script lang="ts">
	import type { Repo } from '#lib/api.ts';
	import { cloneUrls, refShortName } from '#lib/format.ts';
	import CopyButton from './CopyButton.svelte';

	let { repo }: { repo: Repo } = $props();
	const cmd = $derived(
		`git remote add origin ${cloneUrls(repo.owner, repo.name).ssh}\ngit push -u origin ${refShortName(repo.defaultBranch)}`
	);
</script>

<div class="max-w-2xl rounded-md border border-border bg-surface p-5">
	<h2 class="font-medium">This repository is empty</h2>
	<p class="mt-1 text-[0.8125rem] text-fg-muted">
		Push a first commit over SSH. Every commit must be SSH-signed.
	</p>
	<div class="relative mt-3">
		<!-- svelte-ignore a11y_no_noninteractive_tabindex (scrollable: must be focusable) -->
		<pre
			tabindex="0"
			class="overflow-x-auto rounded-sm bg-surface-2 px-3 py-2 pr-10 font-mono text-xs">{cmd}</pre>
		<CopyButton text={cmd} class="absolute top-1.5 right-1.5 p-1.5 text-fg-muted hover:text-fg" />
	</div>
</div>
