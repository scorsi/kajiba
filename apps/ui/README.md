# kajiba-ui

Web interface of kajiba: a SvelteKit 3 SPA (Svelte 5, TypeScript, Tailwind 4),
built as static files and served by kajiba-server (ADR 0006). See `DESIGN.md`
for the design, the route map and what the remaining mock assumes.

```sh
pnpm install
pnpm dev      # dev server on http://localhost:5173
pnpm check    # svelte-check (0 errors, 0 warnings)
pnpm build    # static SPA in build/ (fallback index.html, precompressed)
```

Try `?mock=slow`, `?mock=error`, `?mock=unauth` or `?mock=empty` on a page still
served by the mock.

Layout:

- `src/lib/api.ts` — the only data module (auth wired to the server, the rest mock)
- `src/lib/mock.ts` — mock data, imported by `api.ts` only
- `src/lib/gen/` — connect-web client generated from `proto/` (`buf generate`)
- `src/lib/styles/tokens.css` — design tokens (colours, fonts, radii)
- `src/lib/components/` — UI components
- `src/routes/(app)/` — signed-in pages with the sidebar shell
- `src/routes/(auth)/` — sign-in (`/-/login`) and enrollment (`/-/enroll#<token>`)
