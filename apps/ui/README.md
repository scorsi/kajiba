# kajiba-ui

Web interface of kajiba: a SvelteKit 3 SPA (Svelte 5, TypeScript, Tailwind 4),
built as static files and served by kajiba-server (ADR 0006). See `DESIGN.md`
for the design and the route map.

```sh
pnpm install
pnpm dev      # dev server on http://localhost:5173
pnpm check    # svelte-check (0 errors, 0 warnings)
pnpm build    # static SPA in build/ (fallback index.html)
```

Layout:

- `src/lib/api.ts` — the only data module (every call goes to the server)
- `src/lib/gen/` — connect-web client generated from `proto/` (`buf generate`)
- `src/lib/styles/tokens.css` — design tokens (colours, fonts, radii)
- `src/lib/components/` — UI components
- `src/routes/(app)/` — signed-in pages with the sidebar shell
- `src/routes/(auth)/` — sign-in (`/-/login`) and enrollment (`/-/enroll#<token>`)
