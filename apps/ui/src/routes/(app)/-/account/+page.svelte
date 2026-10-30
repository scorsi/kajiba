<!-- Account: personal access tokens; declared tokens are read-only. -->
<script lang="ts">
	import {
		TOKEN_SCOPES,
		createToken,
		getViewer,
		listTokens,
		revokeToken,
		type Token
	} from '#lib/api.ts';
	import CopyButton from '#lib/components/CopyButton.svelte';
	import Icon from '#lib/components/Icon.svelte';
	import Skeleton from '#lib/components/Skeleton.svelte';
	import StateView from '#lib/components/StateView.svelte';
	import { absoluteTime, relativeTime, toDate } from '#lib/format.ts';
	import { resource } from '#lib/resource.svelte.ts';

	const viewer = resource(() => getViewer());
	const tokens = resource(() => listTokens().then((r) => r.tokens));

	const errorText = (e: unknown) => (e instanceof Error ? e.message : String(e));

	// ── tokens ──
	const EXPIRY = [
		{ days: 7, label: '7 days' },
		{ days: 30, label: '30 days' },
		{ days: 90, label: '90 days' },
		{ days: 365, label: '1 year' }
	];
	let showForm = $state(false);
	let form = $state({ name: '', scopes: ['repo:read'] as string[], days: 90 });
	let creating = $state(false);
	let createError = $state('');
	let created = $state<{ token: Token; secret: string } | null>(null);
	let confirmRevoke = $state<string | null>(null);
	let revokeError = $state('');

	const runtimeTokens = $derived(tokens.data?.filter((t) => !t.declared) ?? []);
	const declaredTokens = $derived(tokens.data?.filter((t) => t.declared) ?? []);
	const nameValid = $derived(/^[\w .@:-]{1,64}$/.test(form.name.trim()));

	async function submitToken(e: SubmitEvent) {
		e.preventDefault();
		if (!nameValid || form.scopes.length === 0) return;
		creating = true;
		createError = '';
		try {
			const expire = Math.floor(Date.now() / 1000) + form.days * 86400;
			created = await createToken({
				name: form.name.trim(),
				scopes: form.scopes,
				expireTime: { seconds: BigInt(expire), nanos: 0 }
			});
			showForm = false;
			form = { name: '', scopes: ['repo:read'], days: 90 };
			tokens.retry();
		} catch (err) {
			createError = errorText(err);
		} finally {
			creating = false;
		}
	}

	async function revoke(id: string) {
		revokeError = '';
		try {
			await revokeToken({ id });
			confirmRevoke = null;
			if (created?.token.id === id) created = null;
			tokens.retry();
		} catch (err) {
			revokeError = errorText(err);
		}
	}

	function expiry(t: Token): { text: string; soon: boolean } {
		if (t.revokedTime) return { text: 'revoked', soon: false };
		const d = toDate(t.expireTime);
		if (!d) return { text: 'never expires', soon: false };
		const soon = d.getTime() - Date.now() < 7 * 86400000;
		return { text: `expires ${relativeTime(d)}`, soon };
	}

	const btn =
		'inline-flex h-8 items-center gap-1.5 rounded-sm border border-border bg-surface px-3 text-[0.8125rem] hover:bg-surface-2 disabled:opacity-60';
	const btnPrimary =
		'inline-flex h-8 items-center gap-1.5 rounded-sm bg-accent px-3 text-[0.8125rem] font-medium text-accent-fg hover:opacity-90 disabled:opacity-60';
</script>

{#snippet scopeChips(scopes: string[])}
	<span class="flex flex-wrap gap-1">
		{#each scopes as s (s)}
			<span class="rounded-sm bg-surface-2 px-1 font-mono text-[0.6875rem] text-fg-muted">{s}</span>
		{/each}
	</span>
{/snippet}

<svelte:head><title>Account · kajiba</title></svelte:head>

<main class="mx-auto max-w-4xl px-4 pt-5 pb-16 sm:px-6 lg:px-8 lg:pt-7">
	<header>
		<h1 class="text-xl font-semibold tracking-tight">Account</h1>
		{#if viewer.data}<p class="text-[0.8125rem] text-fg-muted">
				Signed in as <span class="font-medium text-fg">{viewer.data.user}</span>{viewer.data.admin
					? ' · administrator'
					: ''}
			</p>{/if}
	</header>

	<!-- Tokens -->
	<section class="mt-6 rounded-md border border-border bg-surface" aria-labelledby="tokens-h">
		<div class="flex flex-wrap items-center gap-3 border-b border-border px-4 py-3">
			<div class="flex-1">
				<h2 id="tokens-h" class="text-[0.8125rem] font-medium">Personal access tokens</h2>
				<p class="text-xs text-fg-subtle">For kajiba-cli and scripts. HTTPS git uses them too.</p>
			</div>
			{#if !showForm}
				<button
					type="button"
					class={btnPrimary}
					onclick={() => ((showForm = true), (created = null))}
				>
					<Icon name="plus" size={14} /> New token
				</button>
			{/if}
		</div>

		{#if created}
			<div class="border-b border-border bg-ok-soft px-4 py-3" role="status">
				<p class="text-[0.8125rem] font-medium text-ok">
					Token “{created.token.name}” created. Copy it now: it won't be shown again.
				</p>
				<div class="mt-2 flex rounded-sm border border-border bg-surface">
					<input
						readonly
						value={created.secret}
						aria-label="New token secret"
						class="min-w-0 flex-1 bg-transparent px-2 py-1.5 font-mono text-xs"
						onfocus={(e) => e.currentTarget.select()}
					/>
					<CopyButton
						text={created.secret}
						label="Copy token"
						class="border-l border-border px-2.5 text-fg-muted hover:text-fg"
					/>
				</div>
				<button
					type="button"
					class="mt-2 text-xs text-fg-muted hover:underline"
					onclick={() => (created = null)}>I've stored it</button
				>
			</div>
		{/if}

		{#if showForm}
			<form class="space-y-4 border-b border-border px-4 py-4" onsubmit={submitToken}>
				<label class="block text-xs text-fg-subtle">
					Name
					<input
						bind:value={form.name}
						required
						maxlength="64"
						placeholder="e.g. kajiba-cli on laptop"
						aria-invalid={form.name !== '' && !nameValid}
						class="mt-1 h-8 w-full max-w-sm rounded-sm border border-border bg-bg px-2 text-[0.8125rem] text-fg placeholder:text-fg-subtle aria-invalid:border-danger"
					/>
				</label>
				<fieldset>
					<legend class="text-xs text-fg-subtle">Scopes</legend>
					<div class="mt-1 grid gap-1 sm:grid-cols-2">
						{#each TOKEN_SCOPES as s (s.id)}
							<label class="flex items-center gap-2 text-[0.8125rem]">
								<input
									type="checkbox"
									value={s.id}
									bind:group={form.scopes}
									class="accent-(--k-accent)"
								/>
								<span class="font-mono text-xs">{s.id}</span>
								<span class="text-fg-subtle">{s.label}</span>
							</label>
						{/each}
					</div>
				</fieldset>
				<label class="block text-xs text-fg-subtle">
					Expires in
					<select
						bind:value={form.days}
						class="mt-1 block h-8 rounded-sm border border-border bg-bg px-2 text-[0.8125rem] text-fg"
					>
						{#each EXPIRY as e (e.days)}<option value={e.days}>{e.label}</option>{/each}
					</select>
				</label>
				{#if createError}<p class="text-xs text-danger" role="alert">{createError}</p>{/if}
				<div class="flex gap-2">
					<button
						type="submit"
						class={btnPrimary}
						disabled={creating || !nameValid || form.scopes.length === 0}
					>
						{creating ? 'Creating…' : 'Create token'}
					</button>
					<button type="button" class={btn} onclick={() => (showForm = false)}>Cancel</button>
				</div>
			</form>
		{/if}

		{#if tokens.error}
			<StateView error={tokens.error} onretry={tokens.retry} />
		{:else if !tokens.data}
			<div class="p-4"><Skeleton lines={3} /></div>
		{:else}
			{#if revokeError}<p class="px-4 pt-3 text-xs text-danger" role="alert">{revokeError}</p>{/if}
			<ul class="divide-y divide-border">
				{#each runtimeTokens as t (t.id)}
					{@const ex = expiry(t)}
					<li class="flex flex-wrap items-center gap-x-3 gap-y-1.5 px-4 py-2.5 text-[0.8125rem]">
						<span class="min-w-0 flex-1">
							<span class="font-medium">{t.name}</span>
							<span
								class="mt-0.5 flex flex-wrap items-center gap-x-2 gap-y-1 text-xs text-fg-subtle"
							>
								{@render scopeChips(t.scopes)}
								<span class={ex.soon ? 'text-warn' : ''} title={absoluteTime(toDate(t.expireTime))}
									>{ex.text}</span
								>
								<span
									>· {t.lastUsedTime
										? `last used ${relativeTime(toDate(t.lastUsedTime))}`
										: 'never used'}</span
								>
							</span>
						</span>
						{#if confirmRevoke === t.id}
							<span class="flex items-center gap-2">
								<span class="text-xs text-fg-muted">Revoke “{t.name}”?</span>
								<button
									type="button"
									class="inline-flex h-8 items-center rounded-sm bg-danger px-3 text-[0.8125rem] font-medium text-surface"
									onclick={() => revoke(t.id)}>Revoke</button
								>
								<button type="button" class={btn} onclick={() => (confirmRevoke = null)}
									>Cancel</button
								>
							</span>
						{:else if !t.revokedTime}
							<button type="button" class={btn} onclick={() => (confirmRevoke = t.id)}>
								<Icon name="trash" size={14} /> Revoke
							</button>
						{/if}
					</li>
				{:else}
					<li class="px-4 py-4 text-[0.8125rem] text-fg-subtle">No personal token yet.</li>
				{/each}
			</ul>

			{#if declaredTokens.length}
				<h3
					class="flex items-center gap-1.5 border-t border-border bg-surface-2 px-4 py-2 text-xs text-fg-muted"
				>
					<Icon name="lock" size={12} /> Declared in the server configuration
				</h3>
				<ul class="divide-y divide-border">
					{#each declaredTokens as t (t.id)}
						{@const ex = expiry(t)}
						<li class="px-4 py-2.5 text-[0.8125rem]">
							<span class="font-medium">{t.name}</span>
							<span
								class="mt-0.5 flex flex-wrap items-center gap-x-2 gap-y-1 text-xs text-fg-subtle"
							>
								{@render scopeChips(t.scopes)}
								<span class={ex.soon ? 'text-warn' : ''}>{ex.text}</span>
								<span
									>· {t.lastUsedTime
										? `last used ${relativeTime(toDate(t.lastUsedTime))}`
										: 'never used'}</span
								>
							</span>
						</li>
					{/each}
				</ul>
			{/if}
		{/if}
	</section>
</main>
