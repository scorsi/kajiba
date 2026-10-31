<!-- / — Activity, the default page: what happened across the forge. -->
<script lang="ts">
	import Skeleton from '#lib/components/Skeleton.svelte';
	import StateView from '#lib/components/StateView.svelte';
	import { loadForge } from '#lib/data/forge.ts';
	import { resource } from '#lib/resource.svelte.ts';
	import ActivityView from '#lib/views/ActivityView.svelte';

	const forge = resource(() => loadForge());
</script>

<svelte:head><title>Activity · kajiba</title></svelte:head>

{#if forge.error}
	<StateView error={forge.error} onretry={forge.retry} />
{:else if !forge.data}
	<div class="p-6"><Skeleton lines={10} /></div>
{:else}
	<ActivityView forge={forge.data} />
{/if}
