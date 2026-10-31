<!-- /-/admin — Administration, runners included. Administrators only. -->
<script lang="ts">
	import { ApiError, Code, getServerInfo, getViewer } from '#lib/api.ts';
	import Skeleton from '#lib/components/Skeleton.svelte';
	import StateView from '#lib/components/StateView.svelte';
	import { loadForge } from '#lib/data/forge.ts';
	import { resource } from '#lib/resource.svelte.ts';
	import AdminView from '#lib/views/AdminView.svelte';

	const data = resource(async () => {
		const viewer = await getViewer();
		if (!viewer.admin) throw new ApiError(Code.PermissionDenied, 'administrators only');
		const [forge, server] = await Promise.all([loadForge(), getServerInfo()]);
		return { forge, server };
	});
</script>

<svelte:head><title>Administration · kajiba</title></svelte:head>

{#if data.error}
	<StateView error={data.error} onretry={data.retry} what="page" />
{:else if !data.data}
	<div class="p-6"><Skeleton lines={12} /></div>
{:else}
	<AdminView forge={data.data.forge} server={data.data.server} />
{/if}
