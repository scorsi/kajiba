# ADR 0092 — Web application dependencies

Status: Accepted — 2026-10-30

## Context

J4 replaces the placeholder web application with the owner's validated design (0006): Tailwind styling from a single token file, bundled fonts, rendered READMEs and highlighted source files. ADR 0034 requires each direct dependency, its user and its reason to be recorded before code uses it. The owner approved this list on 2026-10-30.

## Decision

- **Amends 0034 and 0076 — web application dependencies of `apps/ui`.**

  | Package                                        | Kind        | Why                                                                                    |
  | ---------------------------------------------- | ----------- | -------------------------------------------------------------------------------------- |
  | `tailwindcss` 4, `@tailwindcss/vite` 4         | development | utility styling compiled at build time over the design tokens; nothing at runtime      |
  | `marked` 18                                    | runtime     | Markdown to HTML for READMEs and Markdown files, loaded lazily                         |
  | `dompurify` 3                                  | runtime     | repository content is untrusted: rendered Markdown is sanitised before it is displayed |
  | `highlight.js` 11 (core and selected grammars) | runtime     | syntax highlighting themed through CSS classes, grammars loaded per file language      |
  | `@fontsource-variable/inter` 5                 | runtime     | bundled UI font (latin subset), the same on every system                               |
  | `@fontsource-variable/jetbrains-mono` 5        | runtime     | bundled code font (latin subset)                                                       |

- The web application talks to the server only through the committed connect-web client (0005, 0020) and the WebAuthn ceremony routes (0084, 0085); the browser's own WebAuthn JSON helpers convert the ceremony options, no WebAuthn library is added.
- Formatting stays with the repository's formatter; the prototype's Prettier plugins are not added.

## Consequences

- The bundle stays a static SPA: the fonts and lazily loaded chunks are files of `packages.kajiba-ui`.
- A later redesign swaps the token file and the views; data access stays behind one module.

## Rejected alternatives

- A component or icon library: the design's own components and inline icons cover every page.
- A server-side Markdown renderer: the server would render untrusted content for a page it does not own.
