<!-- "Clone" disclosure with HTTPS and SSH URLs. Hidden for archived repos (transport refused). -->
<script lang="ts">
	import { cloneUrls } from '#lib/format.ts';
	import CopyButton from './CopyButton.svelte';
	import Icon from './Icon.svelte';
	import Popover from './Popover.svelte';

	let { owner, name }: { owner: string; name: string } = $props();
	const urls = $derived(cloneUrls(owner, name));
</script>

<Popover
	width="26rem"
	triggerClass="h-8 gap-1.5 rounded-sm bg-accent px-3 text-[0.8125rem] font-medium text-accent-fg hover:opacity-90"
>
	{#snippet trigger()}Clone <Icon name="chevron" size={14} />{/snippet}
	<div class="space-y-2 p-3">
		{#each [['HTTPS', urls.https, 'read-only'], ['SSH', urls.ssh, 'read/write, signed commits']] as [label, url, note] (label)}
			<div>
				<div class="mb-1 flex justify-between text-xs text-fg-subtle">
					<span>{label}</span><span>{note}</span>
				</div>
				<div class="flex rounded-sm border border-border">
					<input
						readonly
						value={url}
						aria-label="{label} clone URL"
						class="min-w-0 flex-1 bg-transparent px-2 py-1 font-mono text-xs"
						onfocus={(e) => e.currentTarget.select()}
					/>
					<CopyButton
						text={url}
						label="Copy {label} URL"
						class="border-l border-border px-2 text-fg-muted hover:text-fg"
					/>
				</div>
			</div>
		{/each}
	</div>
</Popover>
