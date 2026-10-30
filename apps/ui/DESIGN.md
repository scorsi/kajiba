# kajiba-frontend — design

The owner's validated design of the kajiba web interface, imported in J4. Views
only import from `src/lib/api.ts`: sign-in, enrollment, sign-out and
`getViewer` talk to kajiba-server; every other function still reads the mock
(`src/lib/mock.ts`) until J5 wires it to the connect-web client.

```sh
pnpm install && pnpm dev     # http://localhost:5173
pnpm check                   # svelte-check, must stay at 0 errors / 0 warnings
pnpm build                   # static SPA in build/, fallback index.html, .gz/.br precompressed
```

Append `?mock=slow|error|unauth|empty` to any URL to see the loading, error,
signed-out and empty states.

## 1. Direction: C — "Ai" (藍), sidebar workbench

A calm, dense tool UI: cool neutral greys, a single indigo accent, sans for
UI and prose, monospace only for ids, paths, refs and code. Navigation lives in
one place, the sidebar; content gets the rest of the width.

How we got here:

| Step                                                                                      | Outcome                                                                                                              |
| ----------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------- |
| Three directions on the repo home (A "Washi" ledger, B "Tetsu" console, C "Ai" workbench) | **C** chosen                                                                                                         |
| Fonts: system stack vs bundled                                                            | **Bundled** Inter + JetBrains Mono (latin subset)                                                                    |
| Three top-bar variants for C (popovers, bar-only with repo switcher, status line)         | Kept the **repo switcher**; no top bar, everything in the sidebar; tilted wordmark badge rejected                    |
| Status line (ambient runner state)                                                        | **Removed**, idea parked (see open questions)                                                                        |
| Runners / stats                                                                           | Split into two pages: **Activity** (git activity dashboard) and **Administration** (runners, mirrors, users, server) |

Layout:

- **Sidebar** (16 rem, sticky; a drawer below `lg` = 1024 px):
  wordmark → repo switcher (`/` opens it, Enter jumps to the first match) →
  _Repository_ section (Code, History, Runs placeholder) when inside a repo →
  _Forge_ section (Repositories, Activity, Administration for admins) →
  footer with the runners summary (dots, links to Administration), account and theme toggle.
- **Repository pages** share a header (owner / name, archived badge,
  description, Clone popover) from the repo layout. Content is two columns at
  `xl` (directory panel 22 rem | README or file), one column below.
- Everything declared in the configuration is shown read-only, with a lock and
  "Declared in the server configuration". No create/edit/delete controls except
  personal tokens and passkeys (runtime by design).

## 2. Tokens

Single file: `src/lib/styles/tokens.css`. Raw values are `--k-*` custom
properties, each written once as `light-dark(<light>, <dark>)`; `app.css`
exposes them to Tailwind (`bg-surface`, `text-fg-muted`, `font-mono`…).
Theme = OS preference, overridden by `data-theme="light|dark"` on `<html>`
(toggle in the sidebar, stored in `localStorage`, applied before first paint
by an inline script in `app.html`).

| Token                                   | Light                                  | Dark                  | Use                                                     |
| --------------------------------------- | -------------------------------------- | --------------------- | ------------------------------------------------------- |
| `--k-bg`                                | `#f6f7f9`                              | `#101217`             | page                                                    |
| `--k-surface` / `-2`                    | `#ffffff` / `#eef0f4`                  | `#161921` / `#1d212b` | cards / insets, hover                                   |
| `--k-border` / `-strong`                | `#dde1e8` / `#c3c9d3`                  | `#2a2f3b` / `#3b4251` | hairlines                                               |
| `--k-fg`                                | `#161a22`                              | `#e3e6ec`             | text, 16.5:1 / 14.6:1                                   |
| `--k-fg-muted`                          | `#4f5868`                              | `#a8afbd`             | secondary text, 6.9:1 / 8.4:1                           |
| `--k-fg-subtle`                         | `#5f6878`                              | `#8a93a3`             | tertiary text, 5.3:1 / 5.9:1 (AA)                       |
| `--k-accent` (+`-fg`, `-soft`)          | `#2f4f8f`                              | `#8fa9e6`             | indigo: active nav, primary buttons, bars               |
| `--k-link`, `--k-focus`                 | `#2a4a8a`, `#3a5fb0`                   | `#9db4ec`, `#8fa9e6`  | links, 2 px focus ring                                  |
| `--k-ok` / `warn` / `danger` (+`-soft`) | green / amber / red                    | lighter tints         | status only, always with icon + label                   |
| `--k-syn-*`                             | 8 hues                                 | 8 hues                | highlight.js classes (`syntax.css`), ≥ 4.5:1 on surface |
| `--k-line-highlight`                    | `#fff4cc`                              | `#3a3418`             | selected source lines                                   |
| `--k-font-sans`                         | Inter Variable → system UI             |                       | UI and prose                                            |
| `--k-font-mono`                         | JetBrains Mono Variable → ui-monospace |                       | ids, paths, code (ligatures off in code)                |
| `--k-radius` / `-lg`                    | 6 px / 8 px                            |                       |                                                         |

Type scale: body 14 px (`0.875rem`), dense UI 13 px (`0.8125rem`), meta 12 px,
page titles 20 px. Contrast checked with axe-core (WCAG 2 A/AA, light and dark,
1280 px and 360 px): no violations.

## 3. Components (`src/lib/components/`)

| Component            | Role                                                                                        |
| -------------------- | ------------------------------------------------------------------------------------------- |
| `Sidebar`            | All navigation (see above)                                                                  |
| `RepoSwitcher`       | Filterable repository list in a popover, `/` shortcut                                       |
| `Popover`            | `<details>`-based disclosure, Escape and click-outside close                                |
| `Wordmark`           | "kajiba 鍛冶場" text                                                                        |
| `ThemeToggle`        | system → light → dark                                                                       |
| `CloneButton`        | HTTPS + SSH URLs with copy (hidden for archived / empty repos)                              |
| `RefSelect`          | Branch/tag picker (native `<select>` with optgroups)                                        |
| `FileList`           | Directory panel: ref, latest commit, entries, `..`, active file                             |
| `RepoDetails`        | Default branch, CI, former names, access, mirrors (read-only)                               |
| `EmptyRepo`          | First-push instructions                                                                     |
| `Breadcrumbs`        | Path inside a repo                                                                          |
| `FileView`           | File header + actions; text, Markdown (rendered/source), image, binary, large file, symlink |
| `CodeView`           | Line numbers as `#L12` / `#L12-L20` anchors (shift-click), lazy highlighting                |
| `Markdown`           | Sanitised Markdown, highlighted code blocks                                                 |
| `CommitRow`          | One commit in History (expandable body, merge badge, copy id)                               |
| `DailyBars`          | Single-series column chart with hover tooltip and table view                                |
| `RunnerState`        | State dot + label (never colour alone)                                                      |
| `StateView`          | Not found / sign-in required / no access / unreachable + retry                              |
| `Skeleton`           | Loading placeholder                                                                         |
| `CopyButton`, `Icon` | Clipboard with live announcement; 28 inline stroke icons (24 px grid)                       |

Non-component modules: `api.ts` (data), `loaders.ts` (RPC composition),
`resource.svelte.ts` (async data bound to reactive inputs, stale-response safe),
`context.ts` (current repo), `repo-ui.ts` (URLs, labels, ref/path splitting),
`format.ts` (pure formatting), `highlight.ts`, `markdown.ts`, `theme.svelte.ts`,
`src/params.ts` (`slug` matcher for owner/name).

## 4. Routes → components

| Route                               | File                                                               | Main components                                                       | RPCs                                                                        |
| ----------------------------------- | ------------------------------------------------------------------ | --------------------------------------------------------------------- | --------------------------------------------------------------------------- |
| `/`                                 | `(app)/+page.svelte`                                               | list grouped by owner, filter, show archived                          | ListRepos                                                                   |
| `/<owner>/<name>`                   | `(app)/[owner=slug]/[name=slug]/+page.svelte` (+ `+layout.svelte`) | FileList, RepoDetails, Markdown, EmptyRepo, CloneButton               | ListRepos, _ListRefs_, GetTree, ListCommits(1), GetBlob(README)             |
| `/<owner>/<name>/tree/<ref>/<path>` | `…/tree/[...rest]/+page.svelte`                                    | Breadcrumbs, FileList, FileView/CodeView or README                    | _ListRefs_, GetTree ×2, GetBlob, ListCommits(1)                             |
| `/<owner>/<name>/commits/<ref>`     | `…/commits/[...ref]/+page.svelte`                                  | RefSelect, CommitRow, cursor pagination (`?after=&page=`)             | ListCommits                                                                 |
| `/<owner>/<name>/commit/<id>`       | `…/commit/[id]/+page.svelte`                                       | metadata, changed files (A/M/D/R)                                     | ListCommits(revision=id, 1), GetCommitDiff                                  |
| `/-/runs/<id>`                      | `(app)/-/runs/[id]/+page.svelte`                                   | **placeholder** (steps + log skeleton)                                | _ListRunners_ (to show repo/ref if running)                                 |
| `/-/activity`                       | `(app)/-/activity/+page.svelte`                                    | KPIs, DailyBars, per repo/author, latest commits                      | _GetActivity_                                                               |
| `/-/admin`                          | `(app)/-/admin/+page.svelte`                                       | runners, push mirrors, users, server                                  | _ListRunners_, _ListMirrorStatus_, _GetServerInfo_, ListRepos               |
| `/-/account`                        | `(app)/-/account/+page.svelte`                                     | passkeys (re-enrol, add), tokens (create, revoke, declared read-only) | _ListPasskeys_, _EnrollPasskey_, _ListTokens_, _CreateToken_, _RevokeToken_ |
| `/-/login`                          | `(auth)/-/login/+page.svelte`                                      | one button, `?next=` return                                           | _SignInWithPasskey_                                                         |
| `/-/enroll#<token>`                 | `(auth)/-/enroll/+page.svelte`                                     | link check, first passkey                                             | _GetEnrollment_, _CompleteEnrollment_                                       |
| anything else                       | `+error.svelte`                                                    | 404                                                                   |                                                                             |

_Italic_ = not in the API yet (section 7). Every page has loading (skeleton),
empty, error (StateView with retry) and signed-out states; archived repos get a
badge and a banner, and their Clone button is hidden.

Old repository names (`previousNames`) redirect to the current name, keeping
the sub-path, with a "Renamed from…" notice.

## 5. Dependencies

| Package                                | Why                                                                                                                                   | Size                                                    | Loaded                               |
| -------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------- | ------------------------------------ |
| `marked` 18                            | Markdown → HTML (GFM tables, task lists); small, well known                                                                           | 13.9 KB gz                                              | lazily, on the first README/Markdown |
| `dompurify` 3                          | README content is untrusted: sanitise before `{@html}`                                                                                | 11.3 KB gz                                              | lazily, with marked                  |
| `highlight.js` 11 (core + 20 grammars) | Smallest mainstream highlighter that themes via CSS classes (tokens), grammars loadable one by one; Shiki is larger and inline-styled | core 8.0 KB gz + 1–7 KB per grammar (rust 2.0, nix 2.1) | lazily, per file language            |
| `@fontsource-variable/inter`           | UI font, consistent across macOS/NixOS (chosen over system stack)                                                                     | 47.1 KB woff2 (latin)                                   | on first paint                       |
| `@fontsource-variable/jetbrains-mono`  | Code font                                                                                                                             | 39.5 KB woff2 (latin)                                   | on first monospace text              |

Dev only: SvelteKit 3.1, Svelte 5.57, Vite 8.3, TypeScript 6.0 (svelte-check 4
does not support TS 7 yet), Tailwind 4.3, adapter-static 4, Prettier.
No component library, no icon font, no state library, no router add-on.

## 6. Bundle sizes (`pnpm build`, gzip -9; fonts are already compressed)

| Page (cold load)                                                    | JS       | CSS    | Fonts   | Total    |
| ------------------------------------------------------------------- | -------- | ------ | ------- | -------- |
| `/-/login`                                                          | 49.4 KB  | 7.5 KB | 47.1 KB | 104.8 KB |
| `/` repository list                                                 | 58.9 KB  | 7.5 KB | 86.6 KB | 153.7 KB |
| `/alice/kajiba` (README: + marked, dompurify, hljs, nix grammar)    | 101.2 KB | 7.5 KB | 86.6 KB | 196.0 KB |
| file view `main.rs` (+ hljs, rust grammar)                          | 77.5 KB  | 7.5 KB | 86.6 KB | 172.3 KB |
| whole `_app/` (every chunk, incl. all grammars and the 8.1 KB mock) | 152.9 KB | 7.5 KB | 86.6 KB |          |

All pages stay under the 300 KB budget even counting fonts. The mock data
(8.1 KB) disappears with the real client; the connect-web runtime will add
some back.

## 7. Mock assumptions = questions for the integrator

Field names follow protobuf-es (camelCase, numeric enums, `Timestamp` as
`{ seconds: bigint, nanos }`). Errors: `ApiError` has the same `.code` numbers as
`ConnectError` (`NotFound` 5, `PermissionDenied` 7, `FailedPrecondition` 9,
`Unavailable` 14, `Unauthenticated` 16).

About the existing RPCs:

1. **Repository reference in requests** is a string `"owner/name"`. Is it a
   string, a message `{ owner, name }`, or `publicId`?
2. **No `GetRepo`**: `findRepo()` pages through `ListRepos` (cached per session).
   Fine for a few dozen repos; a `GetRepo` would be cleaner and would resolve
   `previousNames` server-side.
3. **`revision`** accepts a full ref, a short branch/tag name or a full commit id.
   Short commit ids are not assumed.
4. **Enum names**: `Role.READ/WRITE/ADMIN`, `EntryKind.BLOB/TREE/SYMLINK/SUBMODULE`,
   `ChangeKind.ADDED/MODIFIED/DELETED/RENAMED`, field `Change.kind` (the spec
   says "change kind"). For renames, the old path is not in the API.
5. `GetTree` entries have no size or mode; `GetBlob` returns the whole content.
   The UI caps display at 512 KiB (download / "show anyway") and highlighting
   at 256 KiB. A size on entries, or a streamed/range blob, would avoid
   downloading huge files.
6. **`ListCommits` has no path filter**: "latest commit" on a sub-directory and
   a file's "History" link show the ref's history, not the path's.
7. **No commit lookup**: the commit page uses `ListCommits(revision=id, pageSize=1)`.
   Route `/<owner>/<name>/commit/<id>` is ours (the ADR route list only has
   `/commits/<ref>`).
8. Symlink: `GetBlob` on a symlink returns the target path (git's blob content).
   Submodule: only the commit id is known, no URL.
9. Clone URLs are built from `location.host` + the ADR SSH form
   (`kajiba-git@host:2222`). Should the server advertise them (proxy, other port)?

Not in the API at all (marked _ASSUMED_ in `api.ts`):

10. **`ListRefs(repo)` → `refs[] { name, commitId }`**: needed by the ref
    switcher, and to split `/tree/<ref>/<path>` when the ref has slashes
    (longest matching branch, then tag; ambiguity between branch and tag of the
    same name resolves to the branch). Also the only way to detect an empty repo.
11. **`GetViewer()` → `{ user, admin }`**: who is signed in, and whether to
    show Administration. Unauthenticated → `Code.Unauthenticated`.
12. **Account**: `ListPasskeys`, `EnrollPasskey({ name, replaceId? })`
    (wraps begin → `navigator.credentials.create` → finish), `ListTokens`
    (`Token { id, name, scopes[], createTime, expireTime?, lastUsedTime?, declared }`),
    `CreateToken({ name, scopes, expireTime })` → `{ token, secret }` shown once,
    `RevokeToken({ id })` (refused for declared tokens).
    Scopes: `repo:read`, `repo:write`, `ci:trigger`, `admin` (ADR 0014).
    Expiry choices: 7, 30, 90, 365 days; no "never" for runtime tokens.
13. **Auth**: `SignInWithPasskey()` (begin → `navigator.credentials.get` →
    finish, sets the session), `GetEnrollment({ token })` → `{ user, expireTime }`
    without consuming the link, `CompleteEnrollment({ token, name })`. Resolved
    in J4: the WebAuthn routes of ADRs 0084/0085, link `/-/enroll#<token>`.
14. **Activity**: `GetActivity({ days })` → per-day counts (UTC days, default
    branches), per repo, per author, latest commits. Could be computed in the
    browser from `ListCommits` on each repo, at the cost of many calls.
15. **Administration**: `ListRunners` (name, host, backend, labels, state
    IDLE/BUSY/OFFLINE, lastSeenTime, currentRun), `ListMirrorStatus` (state
    OK/FAILING/DISABLED, last attempt/success, last error, attempts, next retry),
    `GetServerInfo` (version, commit, startTime, configGeneration, database and
    storage sizes, declared token count, users). User role counts are computed
    from repository permissions.

## 8. Open questions

- **Reserved names**: resolved by ADR 0086, runs live at `/-/runs/<id>`.
- **Re-enrol semantics**: does re-enrolling a passkey revoke the old
  credential (as the mock does), or only add a new one? Can a passkey be removed?
- **Status line / ambient state**: idea parked by the owner. Not like the
  prototype (a bottom bar with runners), and not with those figures. To revisit.
- **Runs list per repo**: the sidebar has a "Runs" entry (disabled, "soon"); only
  `/-/runs/<id>` is specified. Where does the list of runs of a repo live?
- **Who can see Activity**: everything the viewer can read (as mocked), or
  admins only?
- **Unified diff**: nice-to-have; needs a `GetCommitDiff` option or a new RPC
  returning hunks. The commit page has a placeholder line for it.
- **Signed commits badge**: not shown yet; would need the signature status
  (and key) per commit in `ListCommits`.
- **Shared style library**: tokens are self-contained in one file, ready to be
  extracted into a design-system package shared by other fronts.
- **Font loading**: fonts load on first use (`font-display: swap`). A `preload`
  for Inter would remove the swap on cold loads; it needs the hashed URL
  injected at build time.

## 9. Accessibility and quality checks done

- axe-core (WCAG 2 A + AA) on every route and state, light and dark, at 1280
  and 360 px: 0 violations. No horizontal page scroll at 360 px.
- Skip link, landmarks, `aria-current` on navigation, visible 2 px focus ring,
  status never conveyed by colour alone, `prefers-reduced-motion` respected
  (pulses are `motion-safe`), scrollable code regions focusable.
- Charts: single hue, no legend (title names the series), hover tooltip and a
  "Show as table" alternative.
- No browser dialogs (`confirm`/`alert`): destructive actions confirm inline.
