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
			await goto(next, { replace: true });
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		} finally {
			pending = false;
		}
	}
</script>

<svelte:head><title>Sign in · kajiba</title></svelte:head>

<section class="border-b border-border bg-surface px-4 py-8 sm:px-6 sm:py-12" aria-label="Sign in">
	<div class="max-w-md">
		<p class="text-xs tracking-wide text-fg-subtle uppercase">▾ Sign in</p>
		<h1 class="mt-1 text-2xl font-semibold tracking-tight">Welcome back</h1>
		<p class="mt-1 text-[0.8125rem] text-fg-muted">
			Use the passkey enrolled for your account. No password, no user name: the key says who you
			are.
		</p>

		<button
			type="button"
			onclick={signIn}
			disabled={pending}
			class="mt-6 inline-flex h-10 items-center justify-center gap-2 rounded-sm bg-accent px-5 font-medium text-accent-fg hover:opacity-90 disabled:opacity-70"
		>
			<Icon name="key" size={16} />
			{pending ? 'Waiting for your passkey…' : 'Sign in with a passkey'}
		</button>

		{#if error}
			<p class="mt-3 rounded-sm bg-danger-soft px-3 py-2 text-[0.8125rem] text-danger" role="alert">
				{error}
			</p>
		{/if}
		{#if next !== '/'}
			<p class="mt-3 text-xs text-fg-subtle">
				Then back to <span class="font-mono text-fg-muted">{next}</span>
			</p>
		{/if}
	</div>
</section>

<p
	class="flex flex-wrap items-center gap-x-3 gap-y-1 border-b border-border px-4 py-2 text-xs text-fg-muted sm:px-6"
>
	<span class="tracking-wide text-fg-subtle uppercase">No account yet</span>
	<span>Accounts are declared by the administrator, who sends you a one-time enrolment link.</span>
</p>
