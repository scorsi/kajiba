<!-- Read-only repository facts, all declared in the server configuration. -->
<script lang="ts">
	import { CiMode, type Repo } from '#lib/api.ts';
	import { refShortName } from '#lib/format.ts';
	import { ROLE_LABEL } from '#lib/repo-ui.ts';
	import Icon from './Icon.svelte';

	let { repo }: { repo: Repo } = $props();
</script>

<section
	class="rounded-md border border-border bg-surface p-3 text-[0.8125rem]"
	aria-label="Details"
>
	<dl class="grid grid-cols-[auto_minmax(0,1fr)] gap-x-4 gap-y-1.5">
		<dt class="text-fg-subtle">Default</dt>
		<dd class="font-mono text-xs leading-5">{refShortName(repo.defaultBranch)}</dd>
		<dt class="text-fg-subtle">CI</dt>
		<dd>
			{repo.ciMode === CiMode.PUSH ? 'On push' : repo.ciMode === CiMode.MANUAL ? 'Manual' : 'Off'}
		</dd>
		{#if repo.previousNames.length}
			<dt class="text-fg-subtle">Formerly</dt>
			<dd class="font-mono text-xs leading-5">{repo.previousNames.join(', ')}</dd>
		{/if}
		<dt class="text-fg-subtle">Access</dt>
		<dd>
			{#each repo.permissions as p (p.user)}
				<div>{p.user} <span class="text-fg-subtle">· {ROLE_LABEL[p.role]}</span></div>
			{/each}
		</dd>
		{#each repo.mirrors as m (m.remoteUrl)}
			<dt class="text-fg-subtle">Mirror</dt>
			<dd>
				<div class="font-mono text-xs break-all">{m.remoteUrl}</div>
				<div class="text-xs text-fg-subtle">{m.enabled ? 'Push mirror' : 'Disabled'}</div>
			</dd>
		{/each}
	</dl>
	<p class="mt-3 flex items-center gap-1.5 border-t border-border pt-2 text-xs text-fg-subtle">
		<Icon name="lock" size={12} /> Declared in the server configuration
	</p>
</section>
