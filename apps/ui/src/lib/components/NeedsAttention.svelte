<!-- Shared attention: unresolved default-branch failures, distinct active runs,
     failing mirrors and offline runners; fixed failures can be revealed. -->
<script lang="ts">
	import { preferences } from '#lib/viewer-preferences.svelte.ts';
	import RunRef from '#lib/components/RunRef.svelte';
	import Icon from '#lib/components/Icon.svelte';
	import { relativeTime, toDate } from '#lib/format.ts';
	import RunBadge from '#lib/components/RunBadge.svelte';
	import { repoHref } from '#lib/data/forge.ts';
	import { attention } from '#lib/data/events.ts';
	import type { Forge } from '#lib/data/forge.ts';
	import { RunState } from '#lib/api.ts';

	let {
		forge,
		runnerHref = (name: string) => `/-/admin#runner-${name}`,
		mirrorHref = (repo: string) => repoHref(repo)
	}: {
		forge: Forge;
		runnerHref?: (name: string) => string;
		mirrorHref?: (repo: string) => string;
	} = $props();

	const a = $derived(attention(forge, preferences.hideFixedFailures));
</script>

{#if a.branches.length || a.mirrors.length || a.runners.length || a.active.length}
	<div class="mt-3 flex flex-wrap items-center gap-x-4 gap-y-1 text-xs" role="status">
		<span class="tracking-wide text-fg-subtle uppercase">Needs attention</span>
		{#each a.branches as b (b.key)}
			<a href="/-/runs/{b.run.id}" class="flex items-center gap-1.5 {b.fixed ? 'text-fg-muted' : 'text-danger'} hover:underline"
				><RunBadge state={RunState.FAILED} size={10} /><span class="font-mono">{b.key}</span>
				{b.branch} {b.fixed ? 'fixed' : 'failing'} · #{b.run.short}</a
			>
		{/each}
		{#if a.active.length}<span class="tracking-wide text-accent uppercase">In progress</span>{/if}
		{#each a.active as r (r.id)}
			<a href="/-/runs/{r.id}" class="flex items-center gap-1.5 text-accent hover:underline">
				<RunBadge state={r.state} label size={10} /><span class="font-mono">{r.repo}</span>
				<RunRef run={r} /> · #{r.short}
			</a>
		{/each}
		{#each a.mirrors as m (m.repo + m.remoteUrl)}
			<a
				href={mirrorHref(m.repo)}
				class="flex items-center gap-1.5 text-danger hover:underline"
				title={m.lastError}
				><Icon name="mirror" size={11} />mirror of <span class="font-mono">{m.repo}</span> · {m.attempts}
				attempts</a
			>
		{/each}
		{#each a.runners as r (r.name)}
			<a href={runnerHref(r.name)} class="flex items-center gap-1.5 text-fg-muted hover:underline"
				><Icon name="server" size={11} /><span class="font-mono">{r.name}</span> offline · {relativeTime(
					toDate(r.lastSeenTime)
				)}</a
			>
		{/each}
	</div>
{/if}
