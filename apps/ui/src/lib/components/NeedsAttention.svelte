<!--
	"Needs attention": current states someone should act on — a default branch
	whose last finished run failed, a failing push mirror, an offline runner.
	Renders nothing when all is well. Shared by Activity and Administration.
-->
<script lang="ts" module>
	import { MirrorState, RunState, RunnerState } from '#lib/api.ts';
	import { refShortName } from '#lib/format.ts';
	import type { Forge } from '#lib/data/forge.ts';

	export function attention(forge: Forge) {
		return {
			branches: forge.repos.flatMap((r) => {
				const last = r.runs.find(
					(x) => x.ref === r.repo.defaultBranch && x.state !== RunState.RUNNING
				);
				return last?.state === RunState.FAILED
					? [{ key: r.key, branch: refShortName(r.repo.defaultBranch), run: last }]
					: [];
			}),
			mirrors: forge.mirrors.filter((m) => m.state === MirrorState.FAILING),
			runners: forge.runners.filter((r) => r.state === RunnerState.OFFLINE && !r.disabled)
		};
	}
</script>

<script lang="ts">
	import Icon from '#lib/components/Icon.svelte';
	import { relativeTime, toDate } from '#lib/format.ts';
	import RunBadge from '#lib/components/RunBadge.svelte';
	import { repoHref } from '#lib/data/forge.ts';

	let {
		forge,
		runnerHref = (name: string) => `/-/admin#runner-${name}`,
		mirrorHref = (repo: string) => repoHref(repo)
	}: {
		forge: Forge;
		runnerHref?: (name: string) => string;
		mirrorHref?: (repo: string) => string;
	} = $props();

	const a = $derived(attention(forge));
</script>

{#if a.branches.length || a.mirrors.length || a.runners.length}
	<div class="mt-3 flex flex-wrap items-center gap-x-4 gap-y-1 text-xs" role="status">
		<span class="tracking-wide text-fg-subtle uppercase">Needs attention</span>
		{#each a.branches as b (b.key)}
			<a href="/-/runs/{b.run.id}" class="flex items-center gap-1.5 text-danger hover:underline"
				><RunBadge state={RunState.FAILED} size={10} /><span class="font-mono">{b.key}</span>
				{b.branch} failing · #{b.run.short}</a
			>
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
