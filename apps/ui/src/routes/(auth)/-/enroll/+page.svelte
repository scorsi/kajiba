<!--
	First enrollment from the one-time link `/-/enroll#<token>` (ADR 0084): the token
	is captured from the fragment and removed from history. Check the link, register the
	first passkey, then continue signed in.
-->
<script lang="ts">
	import { completeEnrollment, getEnrollment, type Viewer } from '#lib/api.ts';
	import Icon from '#lib/components/Icon.svelte';
	import Skeleton from '#lib/components/Skeleton.svelte';
	import { takeEnrollmentToken } from '#lib/enrollment.ts';
	import { resource } from '#lib/resource.svelte.ts';

	const token = takeEnrollmentToken();
	const link = resource(() =>
		token ? getEnrollment({ token }) : Promise.reject(new Error('This link has no token.'))
	);

	let name = $state('');
	let pending = $state(false);
	let error = $state('');
	let done = $state<Viewer | null>(null);

	async function submit(e: SubmitEvent) {
		e.preventDefault();
		pending = true;
		error = '';
		try {
			done = await completeEnrollment({ enrollment: link.data!, name: name.trim() });
		} catch (err) {
			error = err instanceof Error ? err.message : String(err);
		} finally {
			pending = false;
		}
	}
</script>

<svelte:head><title>Enroll a passkey · kajiba</title></svelte:head>

<section
	class="border-b border-border bg-surface px-4 py-8 sm:px-6 sm:py-12"
	aria-label="Enrollment"
>
	<div class="max-w-md">
		<p class="text-xs tracking-wide text-fg-subtle uppercase">▾ Enrollment</p>
		{#if done}
			<h1 class="mt-1 flex items-center gap-2 text-2xl font-semibold tracking-tight">
				<Icon name="check" size={22} class="text-ok" />Passkey registered
			</h1>
			<p class="mt-1 text-[0.8125rem] text-fg-muted">
				You're signed in as <span class="font-mono text-fg">{done.user}</span>. Enroll a second
				passkey from your account as a backup.
			</p>
			<a
				href="/"
				class="mt-6 inline-flex h-10 items-center justify-center rounded-sm bg-accent px-5 font-medium text-accent-fg hover:opacity-90"
				>Continue to kajiba</a
			>
		{:else if link.error}
			<h1 class="mt-1 flex items-center gap-2 text-2xl font-semibold tracking-tight">
				<Icon name="alert" size={22} class="text-danger" />This link can't be used
			</h1>
			<p class="mt-1 text-[0.8125rem] text-fg-muted">
				{link.error instanceof Error ? link.error.message : 'Invalid enrollment link.'} Ask the administrator
				for a new one.
			</p>
			<a href="/-/login" class="mt-4 inline-block text-[0.8125rem] text-link hover:underline"
				>Already enrolled? Sign in</a
			>
		{:else if !link.data}
			<div class="mt-2"><Skeleton lines={4} /></div>
		{:else}
			<h1 class="mt-1 text-2xl font-semibold tracking-tight">
				Welcome, <span class="font-mono">{link.data.user}</span>
			</h1>
			<p class="mt-1 text-[0.8125rem] text-fg-muted">
				Register a passkey to finish setting up your account. You'll sign in with it from now on.
			</p>
			<form class="mt-6" onsubmit={submit}>
				<label class="block max-w-xs text-xs text-fg-subtle">
					Passkey name <span class="text-fg-subtle">(optional)</span>
					<input
						bind:value={name}
						maxlength="64"
						placeholder="e.g. YubiKey 5C"
						class="mt-1 h-9 w-full rounded-sm border border-border bg-bg px-2 text-[0.8125rem] text-fg placeholder:text-fg-subtle"
					/>
				</label>
				<button
					type="submit"
					disabled={pending}
					class="mt-4 inline-flex h-10 items-center justify-center gap-2 rounded-sm bg-accent px-5 font-medium text-accent-fg hover:opacity-90 disabled:opacity-70"
				>
					<Icon name="key" size={16} />
					{pending ? 'Waiting for your passkey…' : 'Create passkey'}
				</button>
				{#if error}
					<p
						class="mt-3 rounded-sm bg-danger-soft px-3 py-2 text-[0.8125rem] text-danger"
						role="alert"
					>
						{error}
					</p>
				{/if}
			</form>
		{/if}
	</div>
</section>
