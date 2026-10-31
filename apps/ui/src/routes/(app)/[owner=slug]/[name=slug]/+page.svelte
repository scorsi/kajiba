<!--
	/<owner>/<name> — the repository view. Everything else (path, ref, time
	travel, folded bands) is in the query string, so one route serves the
	README, files, history and commits. A previous name redirects to the
	current one (declared renames).
-->
<script lang="ts">
	import { goto } from '$app/navigation';
	import { page } from '$app/state';
	import Skeleton from '#lib/components/Skeleton.svelte';
	import StateView from '#lib/components/StateView.svelte';
	import { findRepo } from '#lib/loaders.ts';
	import { resource } from '#lib/resource.svelte.ts';
	import RepoView from '#lib/views/RepoView.svelte';

	const found = resource(() => {
		const owner = page.params.owner ?? '';
		const name = page.params.name ?? '';
		return findRepo(owner, name);
	});
	const repoKey = $derived(found.data ? `${found.data.repo.owner}/${found.data.repo.name}` : '');

	$effect(() => {
		if (found.data?.renamedFrom) goto(`/${repoKey}${page.url.search}`, { replace: true });
	});
</script>

<svelte:head
	><title>{repoKey || `${page.params.owner}/${page.params.name}`} · kajiba</title></svelte:head
>

{#if found.error}
	<StateView error={found.error} onretry={found.retry} what="repository" />
{:else if !found.data || found.data.renamedFrom}
	<div class="p-6"><Skeleton lines={10} /></div>
{:else}
	{#key repoKey}<RepoView {repoKey} />{/key}
{/if}
