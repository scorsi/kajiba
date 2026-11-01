<script lang="ts">
	import type { Hunk } from '#lib/diff.ts';
	import { highlightDiff } from '#lib/highlight.ts';

	let { hunks, language }: { hunks: Hunk[]; language?: string } = $props();
	let html = $state<(string | undefined)[][]>();
	$effect(() => {
		let cancelled = false;
		html = undefined;
		highlightDiff(hunks, language).then((value) => {
			if (!cancelled) html = value;
		});
		return () => (cancelled = true);
	});
</script>

<table
	class="w-full font-mono text-[0.8125rem] leading-5 [font-variant-ligatures:none]"
>
	{#each hunks as h, hi (hi)}
		<tbody>
			<tr
				><td colspan="3" class="bg-surface-2 px-3 py-0.5 text-xs text-fg-subtle"
					>@@ −{h.oldStart} +{h.newStart} @@</td
				></tr
			>
			{#each h.lines as l, li (li)}
				<tr
					class={l.kind === 'add'
						? 'bg-(--k-syn-add-bg)'
						: l.kind === 'del'
							? 'bg-(--k-syn-del-bg)'
							: ''}
				>
					<td class="w-10 px-2 text-right text-xs text-fg-subtle select-none"
						>{l.old ?? ''}</td
					>
					<td class="w-10 px-2 text-right text-xs text-fg-subtle select-none"
						>{l.new ?? ''}</td
					>
					<td class="pr-4 whitespace-pre"
						><span
							class="inline-block w-4 select-none {l.kind === 'add'
								? 'text-ok'
								: l.kind === 'del'
									? 'text-danger'
									: 'text-fg-subtle'}"
							>{l.kind === 'add' ? '+' : l.kind === 'del' ? '−' : ' '}</span
						>{#if html?.[hi]?.[li] !== undefined}{@html html[hi][li]!}{:else}{l.text}{/if}</td
					>
				</tr>
			{/each}
		</tbody>
	{/each}
</table>
