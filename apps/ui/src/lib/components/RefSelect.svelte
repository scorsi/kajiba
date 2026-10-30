<script lang="ts">
	import type { Ref } from '#lib/api.ts';
	import { refKind, refShortName } from '#lib/format.ts';
	import Icon from './Icon.svelte';

	let {
		refs,
		value,
		onchange,
		class: klass = ''
	}: {
		refs: Ref[];
		value: string;
		onchange?: (ref: string) => void;
		class?: string;
	} = $props();

	const branches = $derived(refs.filter((r) => refKind(r.name) === 'branch'));
	const tags = $derived(refs.filter((r) => refKind(r.name) === 'tag'));
	const isTag = $derived(refKind(value) === 'tag');
</script>

<!--
	Native <select>: keyboard, screen readers and mobile pickers for free.
	A filterable popover can replace it if the ref count grows.
-->
<label class="relative inline-flex items-center {klass}">
	<span class="sr-only">Branch or tag</span>
	<Icon
		name={isTag ? 'tag' : 'branch'}
		size={14}
		class="pointer-events-none absolute left-2 text-fg-subtle"
	/>
	<select
		class="h-full w-full cursor-pointer appearance-none truncate bg-transparent py-1 pr-7 pl-7 font-mono text-[0.8125rem] text-fg"
		{value}
		onchange={(e) => onchange?.(e.currentTarget.value)}
	>
		{#if branches.length}
			<optgroup label="Branches">
				{#each branches as r (r.name)}
					<option value={r.name}>{refShortName(r.name)}</option>
				{/each}
			</optgroup>
		{/if}
		{#if tags.length}
			<optgroup label="Tags">
				{#each tags as r (r.name)}
					<option value={r.name}>{refShortName(r.name)}</option>
				{/each}
			</optgroup>
		{/if}
	</select>
	<Icon name="chevron" size={14} class="pointer-events-none absolute right-2 text-fg-subtle" />
</label>
