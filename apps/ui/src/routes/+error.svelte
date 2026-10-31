<!-- Unmatched routes and unexpected errors, inside the shell. -->
<script lang="ts">
	import { page } from '$app/state';
	import { ApiError, Code } from '#lib/api.ts';
	import Shell from '#lib/components/Shell.svelte';
	import StateView from '#lib/components/StateView.svelte';

	const error = $derived(
		page.status === 404
			? new ApiError(Code.NotFound, 'not found')
			: (page.error ?? new ApiError(Code.Unknown, `HTTP ${page.status}`))
	);
</script>

<svelte:head><title>{page.status} · kajiba</title></svelte:head>

<Shell location={String(page.status)}>
	<StateView {error} />
</Shell>
