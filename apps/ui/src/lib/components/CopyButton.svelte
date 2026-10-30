<script lang="ts">
	import Icon from './Icon.svelte';

	let {
		text,
		label = 'Copy',
		class: klass = ''
	}: { text: string; label?: string; class?: string } = $props();

	let copied = $state(false);
	let timer: ReturnType<typeof setTimeout> | undefined;

	async function copy() {
		try {
			await navigator.clipboard.writeText(text);
			copied = true;
			clearTimeout(timer);
			timer = setTimeout(() => (copied = false), 1500);
		} catch {
			/* clipboard refused (insecure context): the text stays selectable */
		}
	}
</script>

<button
	type="button"
	class="inline-flex items-center justify-center {klass}"
	onclick={copy}
	aria-label={copied ? 'Copied' : label}
	title={copied ? 'Copied' : label}
>
	<Icon name={copied ? 'check' : 'copy'} size={14} />
</button>
<span class="sr-only" aria-live="polite">{copied ? 'Copied to clipboard' : ''}</span>
