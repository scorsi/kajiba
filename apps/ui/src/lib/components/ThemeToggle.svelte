<script lang="ts">
	import Icon from './Icon.svelte';
	import { theme, setTheme, type ThemeChoice } from '#lib/theme.svelte.ts';

	const ORDER: ThemeChoice[] = ['system', 'light', 'dark'];
	const ICON = { system: 'monitor', light: 'sun', dark: 'moon' } as const;

	let { class: klass = '', showLabel = false }: { class?: string; showLabel?: boolean } = $props();

	const next = $derived(ORDER[(ORDER.indexOf(theme.choice) + 1) % ORDER.length]);
</script>

<button
	type="button"
	class="inline-flex items-center gap-1.5 {klass}"
	onclick={() => setTheme(next)}
	title="Theme: {theme.choice} (switch to {next})"
	aria-label="Theme: {theme.choice}. Switch to {next}"
>
	<Icon name={ICON[theme.choice]} size={15} />
	{#if showLabel}<span>{theme.choice}</span>{/if}
</button>
