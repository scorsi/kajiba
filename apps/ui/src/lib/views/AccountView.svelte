<!--
	Account in the bands grammar. Pulse: who you are, what you hold, what
	needs attention (a token about to expire, a passkey unused for a month,
	a single passkey), and your access on the right (declared, read-only).
	Bands: Passkeys (add, remove; both confirmed with an existing passkey first,
	ADR 0143), Tokens (create, copy once, revoke), Declared tokens and Declared
	keys (folded, read-only).
	URL: ?passkeys=0, ?tokens=0, ?declared=1, ?keys=1.
-->
<script lang="ts">
	import {
		TOKEN_SCOPES,
		addPasskey,
		createToken,
		deletePasskey,
		getViewer,
		listPasskeys,
		listTokens,
		revokeToken,
		startPasskeyRegistration,
		stepUp,
		type Registration,
		type Token
	} from '#lib/api.ts';
	import { page } from '$app/state';
	import { Role } from '#lib/api.ts';
	import CopyButton from '#lib/components/CopyButton.svelte';
	import Icon from '#lib/components/Icon.svelte';
	import Skeleton from '#lib/components/Skeleton.svelte';
	import StateView from '#lib/components/StateView.svelte';
	import { absoluteTime, relativeTime, toDate } from '#lib/format.ts';
	import type { Timestamp } from '#lib/api.ts';
	import { allRepos } from '#lib/loaders.ts';
	import { ROLE_LABEL } from '#lib/repo-ui.ts';
	import { resource } from '#lib/resource.svelte.ts';
	import BandStrip from '#lib/components/BandStrip.svelte';
	import { repoHref } from '#lib/data/forge.ts';

	const viewer = resource(() => getViewer());
	const passkeys = resource(() => listPasskeys().then((r) => r.passkeys));
	const tokens = resource(() => listTokens().then((r) => r.tokens));
	const repos = resource(() => allRepos());

	// ── bands (URL) ──
	const q = $derived(page.url.searchParams);
	const pulseOpen = $derived(q.get('pulse') !== '0');
	function hrefFor(next: Record<string, string | null>) {
		const u = new URLSearchParams(q.toString());
		for (const [k, v] of Object.entries(next)) v === null ? u.delete(k) : u.set(k, v);
		return `?${u}`;
	}
	const passkeysOpen = $derived(q.get('passkeys') !== '0');
	const tokensOpen = $derived(q.get('tokens') !== '0');
	const declaredOpen = $derived(q.get('declared') === '1');
	const keysOpen = $derived(q.get('keys') === '1');

	// ── access: the viewer's role on each repository (declared) ──
	const access = $derived(
		(repos.data ?? []).flatMap((r) => {
			const role = r.permissions.find((p) => p.user === viewer.data?.user)?.role;
			return role ? [{ key: `${r.owner}/${r.name}`, role, archived: r.archived }] : [];
		})
	);

	// ── needs attention: tokens about to expire, a passkey unused for long, a single passkey ──
	const MONTH = 30 * 86400000;
	const lastUse = (p: { lastUsedTime?: Timestamp; createTime?: Timestamp }) =>
		toDate(p.lastUsedTime ?? p.createTime);
	const stalePasskeys = $derived(
		(passkeys.data ?? []).filter((p) => Date.now() - (lastUse(p)?.getTime() ?? 0) > MONTH)
	);
	const lastSignIn = $derived(
		(passkeys.data ?? [])
			.map((p) => toDate(p.lastUsedTime)?.getTime() ?? 0)
			.reduce((a, b) => Math.max(a, b), 0)
	);

	const errorText = (e: unknown) => (e instanceof Error ? e.message : String(e));

	// ── passkeys: each change is confirmed with an existing passkey first (ADR 0143) ──
	let busy = $state<string | null>(null); // passkey id being removed, or 'new'
	let passkeyError = $state('');
	let newKeyName = $state('');
	let registration = $state<Registration | null>(null);
	let confirmRemove = $state<string | null>(null);

	async function passkeyStep(id: string, step: () => Promise<void>) {
		busy = id;
		passkeyError = '';
		try {
			await step();
		} catch (e) {
			passkeyError = errorText(e);
		} finally {
			busy = null;
		}
	}

	// Two clicks, one browser prompt each: confirm, then create.
	const confirmAdd = () =>
		passkeyStep('new', async () => {
			await stepUp();
			registration = await startPasskeyRegistration();
		});

	const createPasskey = () =>
		passkeyStep('new', async () => {
			if (!registration) return;
			const started = registration;
			registration = null;
			await addPasskey({ registration: started, name: newKeyName.trim() });
			newKeyName = '';
			passkeys.retry();
		});

	const remove = (id: string) =>
		passkeyStep(id, async () => {
			await stepUp();
			await deletePasskey({ id });
			confirmRemove = null;
			passkeys.retry();
		});

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

	const expiring = $derived((tokens.data ?? []).filter((t) => expiry(t).soon));

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

{#snippet tokenMeta(t: Token)}
	{@const ex = expiry(t)}
	<span class="mt-0.5 flex flex-wrap items-center gap-x-2 gap-y-1 text-xs text-fg-subtle">
		{@render scopeChips(t.scopes)}
		<span class={ex.soon ? 'text-warn' : ''} title={absoluteTime(toDate(t.expireTime))}
			>{ex.text}</span
		>
		<span
			>· {t.lastUsedTime ? `last used ${relativeTime(toDate(t.lastUsedTime))}` : 'never used'}</span
		>
	</span>
{/snippet}

<div class="relative min-h-0 flex-1 overflow-y-auto">
	<!-- Pulse -->
	<BandStrip label="Pulse" open={pulseOpen} href={hrefFor({ pulse: pulseOpen ? '0' : null })}>
		<span>Account</span>
	</BandStrip>
	{#if pulseOpen}
	<section class="border-b border-border bg-surface px-4 py-4 sm:px-6" aria-label="Pulse">
		<div class="grid grid-cols-[minmax(0,1fr)] gap-x-10 gap-y-5 lg:grid-cols-[minmax(0,1fr)_20rem]">
			<div class="min-w-0">
				<h1 class="mt-1 text-2xl font-semibold tracking-tight">
					Account {#if viewer.data}<span class="font-mono text-base font-normal text-fg-muted"
							>{viewer.data.user}</span
						>{#if viewer.data.admin}<span class="ml-2 text-xs font-normal text-fg-subtle"
								>administrator</span
							>{/if}{/if}
				</h1>
				<p class="mt-3 flex flex-wrap items-center gap-x-4 gap-y-1 text-xs text-fg-subtle">
					<span class="flex items-center gap-1.5"
						><Icon name="key" size={11} /><span class="tabular text-fg-muted"
							>{passkeys.data?.length ?? '…'}</span
						> passkeys</span
					>
					{#if lastSignIn}<span>last sign-in {relativeTime(new Date(lastSignIn))}</span>{/if}
					<span
						><span class="tabular text-fg-muted">{runtimeTokens.length}</span> personal tokens</span
					>
					<span
						><span class="tabular text-fg-muted">{declaredTokens.length}</span> declared tokens</span
					>
					<span
						>access to <span class="tabular text-fg-muted">{access.length}</span> repositories</span
					>
				</p>
				{#if expiring.length || stalePasskeys.length || (passkeys.data && passkeys.data.length < 2)}
					<div class="mt-3 flex flex-wrap items-center gap-x-4 gap-y-1 text-xs" role="status">
						<span class="tracking-wide text-fg-subtle uppercase">Needs attention</span>
						{#each expiring as t (t.id)}
							<span class="flex items-center gap-1.5 text-warn"
								><Icon name="clock" size={11} />token “{t.name}” {expiry(t).text}</span
							>
						{/each}
						{#each stalePasskeys as p (p.id)}
							<span class="flex items-center gap-1.5 text-fg-muted"
								><Icon name="key" size={11} />{p.name || 'unnamed passkey'} unused since {relativeTime(
									lastUse(p)
								)} — check it still works</span
							>
						{/each}
						{#if passkeys.data && passkeys.data.length < 2}
							<span class="flex items-center gap-1.5 text-warn"
								><Icon name="alert" size={11} />only one passkey: enrol a backup</span
							>
						{/if}
					</div>
				{/if}
				<p class="mt-3 text-xs text-fg-subtle">
					<Icon name="lock" size={12} class="mr-1 inline-block align-[-2px]" />Your access,
					declared tokens and SSH keys come from the server configuration; passkeys and personal
					tokens are yours to manage here.
				</p>
			</div>

			<!-- Access -->
			<aside aria-label="Access">
				<h2 class="mb-2 text-xs tracking-wide text-fg-subtle uppercase">Access</h2>
				<ul class="space-y-0.5 text-[0.8125rem]">
					{#each access as a (a.key)}
						<li class="flex items-center gap-3">
							<a
								href={repoHref(a.key)}
								class="min-w-0 flex-1 truncate font-mono text-xs hover:underline {a.archived
									? 'text-fg-subtle'
									: 'text-fg-muted'}">{a.key}</a
							>
							<span class="text-xs {a.role === Role.ADMIN ? 'text-accent' : 'text-fg-subtle'}"
								>{ROLE_LABEL[a.role]}</span
							>
						</li>
					{:else}
						<li class="text-xs text-fg-subtle">No repository yet.</li>
					{/each}
				</ul>
			</aside>
		</div>
	</section>
	{/if}

	<!-- Passkeys -->
	<BandStrip
		label="Passkeys"
		open={passkeysOpen}
		href={hrefFor({ passkeys: passkeysOpen ? '0' : null })}
	>
		<span class="tabular">{passkeys.data?.length ?? '…'} enrolled</span>
		<span>the only way to sign in · keep at least two</span>
	</BandStrip>
	{#if passkeysOpen}
		<section class="border-b border-border" aria-label="Passkeys">
			{#if passkeys.error}
				<StateView error={passkeys.error} onretry={passkeys.retry} />
			{:else if !passkeys.data}
				<div class="px-6 py-3"><Skeleton lines={2} /></div>
			{:else}
				<ul class="divide-y divide-border/60">
					{#each passkeys.data as pk (pk.id)}
						{@const last = passkeys.data.length < 2}
						<li
							class="flex flex-wrap items-center gap-x-3 gap-y-1 px-4 py-2.5 text-[0.8125rem] sm:px-6"
						>
							<Icon name="key" size={14} class="text-fg-subtle" />
							<span class="min-w-0 flex-1">
								<span class="font-medium {pk.name ? '' : 'text-fg-muted'}"
									>{pk.name || 'Unnamed passkey'}</span
								>
								<span class="block text-xs text-fg-subtle">
									added <span title={absoluteTime(toDate(pk.createTime))}
										>{relativeTime(toDate(pk.createTime))}</span
									>
									· {pk.lastUsedTime
										? `last used ${relativeTime(toDate(pk.lastUsedTime))}`
										: 'never used'}
								</span>
							</span>
							{#if confirmRemove === pk.id}
								<span class="flex items-center gap-2">
									<span class="text-xs text-fg-muted">Remove “{pk.name || 'Unnamed passkey'}”?</span>
									<button
										type="button"
										class="inline-flex h-8 items-center rounded-sm bg-danger px-3 text-[0.8125rem] font-medium text-surface disabled:opacity-60"
										disabled={busy !== null}
										onclick={() => remove(pk.id)}
										>{busy === pk.id ? 'Waiting for the key…' : 'Confirm and remove'}</button
									>
									<button type="button" class={btn} onclick={() => (confirmRemove = null)}
										>Cancel</button
									>
								</span>
							{:else}
								<button
									type="button"
									class={btn}
									disabled={busy !== null || last}
									onclick={() => (confirmRemove = pk.id)}
									title={last
										? 'Your last passkey cannot be removed'
										: 'Confirm with a passkey, then this one stops working'}
								>
									<Icon name="trash" size={14} /> Remove
								</button>
							{/if}
						</li>
					{:else}
						<li class="px-4 py-3 text-[0.8125rem] text-fg-subtle sm:px-6">No passkey.</li>
					{/each}
				</ul>
				<form
					class="flex flex-wrap items-end gap-2 border-t border-border/60 bg-surface px-4 py-3 sm:px-6"
					onsubmit={(e) => {
						e.preventDefault();
						if (registration) createPasskey();
						else confirmAdd();
					}}
				>
					<label class="max-w-sm min-w-48 flex-1 text-xs text-fg-subtle">
						Name of the new passkey
						<input
							bind:value={newKeyName}
							placeholder="e.g. YubiKey 5 Nano"
							maxlength="64"
							class="mt-1 h-8 w-full rounded-sm border border-border bg-bg px-2 text-[0.8125rem] text-fg placeholder:text-fg-subtle"
						/>
					</label>
					{#if registration}
						<button type="submit" class={btnPrimary} disabled={busy !== null}>
							<Icon name="plus" size={14} />
							{busy === 'new' ? 'Waiting for the key…' : 'Create the new passkey'}
						</button>
						<button type="button" class={btn} onclick={() => (registration = null)}>Cancel</button>
					{:else}
						<button type="submit" class={btn} disabled={busy !== null}>
							<Icon name="plus" size={14} />
							{busy === 'new' ? 'Waiting for the key…' : 'Add a passkey'}
						</button>
					{/if}
					<p class="basis-full text-xs text-fg-subtle">
						{registration
							? 'Confirmed. Now create the new passkey on its authenticator (within five minutes).'
							: 'You first confirm with one of your passkeys.'}
					</p>
					{#if passkeyError}<p class="basis-full text-xs text-danger" role="alert">
							{passkeyError}
						</p>{/if}
				</form>
			{/if}
		</section>
	{/if}

	<!-- Personal tokens -->
	<BandStrip label="Tokens" open={tokensOpen} href={hrefFor({ tokens: tokensOpen ? '0' : null })}>
		<span class="tabular">{runtimeTokens.length} personal</span>
		{#if expiring.length}<span class="tabular text-warn">{expiring.length} expiring soon</span>{/if}
		<span>kajiba-cli, scripts, git over HTTPS</span>
	</BandStrip>
	{#if tokensOpen}
		<section class="border-b border-border" aria-label="Personal tokens">
			{#if created}
				<div class="border-b border-border bg-ok-soft px-4 py-3 sm:px-6" role="status">
					<p class="text-[0.8125rem] font-medium text-ok">
						Token “{created.token.name}” created. Copy it now: it won't be shown again.
					</p>
					<div class="mt-2 flex max-w-xl rounded-sm border border-border bg-surface">
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
				<form
					class="space-y-4 border-b border-border bg-surface px-4 py-4 sm:px-6"
					onsubmit={submitToken}
				>
					<label class="block text-xs text-fg-subtle">
						Name
						<input
							bind:value={form.name}
							required
							maxlength="64"
							placeholder="e.g. kajiba-cli on laptop"
							aria-invalid={form.name !== '' && !nameValid}
							class="mt-1 block h-8 w-full max-w-sm rounded-sm border border-border bg-bg px-2 text-[0.8125rem] text-fg placeholder:text-fg-subtle aria-invalid:border-danger"
						/>
					</label>
					<fieldset>
						<legend class="text-xs text-fg-subtle">Scopes</legend>
						<div class="mt-1 grid max-w-2xl gap-1 sm:grid-cols-2">
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
				<div class="px-6 py-3"><Skeleton lines={3} /></div>
			{:else}
				{#if revokeError}<p class="px-4 pt-3 text-xs text-danger sm:px-6" role="alert">
						{revokeError}
					</p>{/if}
				<ul class="divide-y divide-border/60">
					{#each runtimeTokens as t (t.id)}
						<li
							class="flex flex-wrap items-center gap-x-3 gap-y-1.5 px-4 py-2.5 text-[0.8125rem] sm:px-6"
						>
							<span class="min-w-0 flex-1">
								<span class="font-medium">{t.name}</span>
								{@render tokenMeta(t)}
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
						<li class="px-4 py-3 text-[0.8125rem] text-fg-subtle sm:px-6">
							No personal token yet.
						</li>
					{/each}
				</ul>
				{#if !showForm}
					<div class="border-t border-border/60 bg-surface px-4 py-3 sm:px-6">
						<button
							type="button"
							class={btnPrimary}
							onclick={() => ((showForm = true), (created = null))}
						>
							<Icon name="plus" size={14} /> New token
						</button>
					</div>
				{/if}
			{/if}
		</section>
	{/if}

	<!-- Declared tokens -->
	<BandStrip
		label="Declared tokens"
		open={declaredOpen}
		href={hrefFor({ declared: declaredOpen ? null : '1' })}
	>
		<span class="tabular">{declaredTokens.length}</span>
		<span>read-only, from the server configuration</span>
	</BandStrip>
	{#if declaredOpen}
		<section class="border-b border-border" aria-label="Declared tokens">
			<ul class="divide-y divide-border/60">
				{#each declaredTokens as t (t.id)}
					<li class="flex items-center gap-3 px-4 py-2.5 text-[0.8125rem] sm:px-6">
						<Icon name="lock" size={13} class="text-fg-subtle" />
						<span class="min-w-0 flex-1">
							<span class="font-medium">{t.name}</span>
							{@render tokenMeta(t)}
						</span>
					</li>
				{:else}
					<li class="px-4 py-3 text-[0.8125rem] text-fg-subtle sm:px-6">None declared.</li>
				{/each}
			</ul>
		</section>
	{/if}

	<!-- Declared SSH keys -->
	<BandStrip label="Declared keys" open={keysOpen} href={hrefFor({ keys: keysOpen ? null : '1' })}>
		<span class="tabular">{viewer.data?.keys.length ?? '…'}</span>
		<span>SSH authentication and signing, read-only, from the server configuration</span>
	</BandStrip>
	{#if keysOpen}
		<section class="border-b border-border" aria-label="Declared keys">
			<ul class="divide-y divide-border/60">
				{#each viewer.data?.keys ?? [] as k (k.usage + k.fingerprint)}
					<li class="flex items-center gap-3 px-4 py-2.5 text-[0.8125rem] sm:px-6">
						<Icon name="lock" size={13} class="text-fg-subtle" />
						<span class="min-w-0 flex-1">
							<span class="font-medium">{k.usage === 'signing' ? 'Signing' : 'Authentication'}</span>
							<span class="mt-0.5 block truncate font-mono text-xs text-fg-subtle"
								>{k.keyType} {k.fingerprint}</span
							>
						</span>
					</li>
				{:else}
					<li class="px-4 py-3 text-[0.8125rem] text-fg-subtle sm:px-6">None declared.</li>
				{/each}
			</ul>
		</section>
	{/if}
</div>
