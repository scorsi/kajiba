<!--
	Sign in: one action. `?next=/path` returns there afterwards.
	No password, no username field: the passkey identifies the account.
-->
<script lang="ts">
	import { goto } from '$app/navigation';
	import { page } from '$app/state';
	import { signInWithPasskey } from '#lib/api.ts';
	import Icon from '#lib/components/Icon.svelte';

	let pending = $state(false);
	let error = $state('');

	const next = $derived.by(() => {
		const n = page.url.searchParams.get('next') ?? '/';
		return n.startsWith('/') && !n.startsWith('//') ? n : '/'; // same-origin only
	});

	async function signIn() {
		pending = true;
		error = '';
		try {
			await signInWithPasskey();
			await goto(next, { replaceState: true });
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		} finally {
			pending = false;
		}
	}
</script>

<svelte:head><title>Sign in · kajiba</title></svelte:head>

<div class="rounded-md border border-border bg-surface p-6">
	<h1 class="text-lg font-semibold tracking-tight">Sign in</h1>
	<p class="mt-1 text-[0.8125rem] text-fg-muted">Use the passkey enrolled for your account.</p>

	<button
		type="button"
		onclick={signIn}
		disabled={pending}
		class="mt-5 inline-flex h-10 w-full items-center justify-center gap-2 rounded-sm bg-accent font-medium text-accent-fg hover:opacity-90 disabled:opacity-70"
	>
		<Icon name="key" size={16} />
		{pending ? 'Waiting for your passkey…' : 'Sign in with a passkey'}
	</button>

	{#if error}
		<p class="mt-3 rounded-sm bg-danger-soft px-3 py-2 text-[0.8125rem] text-danger" role="alert">
			{error}
		</p>
	{/if}
</div>

<p class="mt-4 px-1 text-xs text-fg-subtle">
	No account yet? Accounts are declared by the administrator, who sends a one-time enrolment link.
</p>
