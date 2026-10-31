<!--
	The only navigation surface: wordmark, repository switcher, repository
	sections, forge pages, then runners summary, account and theme at the bottom.
-->
<script lang="ts">
	import { goto } from '$app/navigation';
	import { page } from '$app/state';
	import { CiMode, RunnerState, logout, type Repo, type Runner, type Viewer } from '#lib/api.ts';
	import { commitsHref, repoPath } from '#lib/repo-ui.ts';
	import Icon, { type IconName } from './Icon.svelte';
	import RepoSwitcher from './RepoSwitcher.svelte';
	import RunnerStateDot from './RunnerState.svelte';
	import ThemeToggle from './ThemeToggle.svelte';
	import Wordmark from './Wordmark.svelte';

	let {
		repos,
		current,
		viewer,
		runners
	}: { repos: Repo[]; current?: Repo; viewer?: Viewer; runners: Runner[] } = $props();

	const route = $derived(page.route.id ?? '');
	async function signOut() {
		await logout();
		await goto('/-/login');
	}

	const online = $derived(runners.filter((r) => r.state !== RunnerState.OFFLINE).length);

	interface Item {
		label: string;
		icon: IconName;
		href?: string;
		active: boolean;
		note?: string;
	}

	const repoItems = $derived<Item[]>(
		current
			? [
					{
						label: 'Code',
						icon: 'file',
						href: repoPath(current),
						active: /\[name=slug\](\/tree|$)/.test(route)
					},
					{
						label: 'History',
						icon: 'clock',
						href: commitsHref(current, current.defaultBranch),
						active: /\/(commits|commit)\//.test(route)
					},
					{
						label: 'Runs',
						icon: 'play',
						active: false,
						note: current.ciMode > CiMode.OFF ? 'soon' : 'off'
					}
				]
			: []
	);

	const forgeItems = $derived<Item[]>([
		{ label: 'Repositories', icon: 'folder', href: '/', active: route === '/(app)' },
		{ label: 'Activity', icon: 'chart', href: '/-/activity', active: route.endsWith('/activity') },
		...(viewer?.admin
			? [
					{
						label: 'Administration',
						icon: 'server' as const,
						href: '/-/admin',
						active: route.endsWith('/admin')
					}
				]
			: [])
	]);

	const itemClass = (active: boolean) =>
		`flex items-center gap-2.5 rounded-sm px-2 py-1.5 text-[0.8125rem] ${
			active
				? 'bg-accent-soft font-medium text-accent'
				: 'text-fg-muted hover:bg-surface-2 hover:text-fg'
		}`;
</script>

{#snippet section(title: string, items: Item[])}
	<div>
		<h2 class="px-2 pb-1 text-[0.6875rem] font-medium tracking-wide text-fg-subtle uppercase">
			{title}
		</h2>
		<ul class="space-y-px">
			{#each items as item (item.label)}
				<li>
					{#if item.href}
						<a
							href={item.href}
							class={itemClass(item.active)}
							aria-current={item.active ? 'page' : undefined}
						>
							<Icon name={item.icon} size={15} />{item.label}
						</a>
					{:else}
						<span class="flex items-center gap-2.5 px-2 py-1.5 text-[0.8125rem] text-fg-subtle">
							<Icon name={item.icon} size={15} />{item.label}
							{#if item.note}<span class="ml-auto text-[0.6875rem]">{item.note}</span>{/if}
						</span>
					{/if}
				</li>
			{/each}
		</ul>
	</div>
{/snippet}

<div class="flex h-full flex-col">
	<div class="hidden h-14 shrink-0 items-center px-4 lg:flex"><Wordmark /></div>

	<div class="px-3 pt-3 lg:pt-0">
		<RepoSwitcher {repos} {current} />
	</div>

	<nav class="flex-1 space-y-5 overflow-y-auto px-3 py-4" aria-label="Main">
		{#if current}{@render section('Repository', repoItems)}{/if}
		{@render section('Forge', forgeItems)}
	</nav>

	<div class="shrink-0 space-y-1 border-t border-border p-3">
		{#if runners.length}
			<a
				href={viewer?.admin ? '/-/admin#runners' : undefined}
				class="flex items-center gap-2.5 rounded-sm px-2 py-1.5 text-xs text-fg-muted hover:bg-surface-2"
			>
				<span class="flex items-center gap-1" aria-hidden="true">
					{#each runners as r (r.name)}<RunnerStateDot state={r.state} />{/each}
				</span>
				<span class="tabular">{online}/{runners.length} runners online</span>
			</a>
		{/if}
		<div class="flex items-center gap-1">
			{#if viewer}
				<a
					href="/-/account"
					class="flex min-w-0 flex-1 items-center gap-2 rounded-sm px-2 py-1.5 text-[0.8125rem] hover:bg-surface-2 {route.endsWith(
						'/account'
					)
						? 'text-accent'
						: 'text-fg-muted hover:text-fg'}"
				>
					<Icon name="user" size={15} /><span class="truncate">{viewer.user}</span>
				</a>
				<button
					type="button"
					onclick={signOut}
					class="rounded-sm p-1.5 text-fg-muted hover:bg-surface-2 hover:text-fg"
				>
					<Icon name="logout" size={15} label="Sign out" />
				</button>
			{:else}
				<a href="/-/login" class="flex-1 px-2 py-1.5 text-[0.8125rem] text-link hover:underline"
					>Sign in</a
				>
			{/if}
			<ThemeToggle class="rounded-sm p-1.5 text-fg-muted hover:bg-surface-2 hover:text-fg" />
		</div>
	</div>
</div>
