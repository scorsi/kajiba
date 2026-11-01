/**
 * Syntax highlighting with highlight.js core + per-language modules, all
 * loaded on demand: nothing here is in the initial bundle, and a page only
 * downloads the grammar it needs.
 */

import type { HLJSApi, LanguageFn } from "highlight.js";

type Loader = () => Promise<{ default: LanguageFn }>;

/** Grammar loaders by highlight.js language name. Add a line to support one more. */
const LANGUAGES: Record<string, Loader> = {
  bash: () => import("highlight.js/lib/languages/bash"),
  css: () => import("highlight.js/lib/languages/css"),
  diff: () => import("highlight.js/lib/languages/diff"),
  dockerfile: () => import("highlight.js/lib/languages/dockerfile"),
  elixir: () => import("highlight.js/lib/languages/elixir"),
  go: () => import("highlight.js/lib/languages/go"),
  ini: () => import("highlight.js/lib/languages/ini"), // also TOML
  javascript: () => import("highlight.js/lib/languages/javascript"),
  json: () => import("highlight.js/lib/languages/json"),
  makefile: () => import("highlight.js/lib/languages/makefile"),
  markdown: () => import("highlight.js/lib/languages/markdown"),
  nim: () => import("highlight.js/lib/languages/nim"),
  nix: () => import("highlight.js/lib/languages/nix"),
  protobuf: () => import("highlight.js/lib/languages/protobuf"),
  python: () => import("highlight.js/lib/languages/python"),
  rust: () => import("highlight.js/lib/languages/rust"),
  sql: () => import("highlight.js/lib/languages/sql"),
  typescript: () => import("highlight.js/lib/languages/typescript"),
  xml: () => import("highlight.js/lib/languages/xml"), // also HTML, SVG, Svelte
  yaml: () => import("highlight.js/lib/languages/yaml"),
};

const BY_EXTENSION: Record<string, string> = {
  sh: "bash",
  bash: "bash",
  zsh: "bash",
  css: "css",
  diff: "diff",
  patch: "diff",
  ex: "elixir",
  exs: "elixir",
  go: "go",
  toml: "ini",
  ini: "ini",
  conf: "ini",
  js: "javascript",
  mjs: "javascript",
  cjs: "javascript",
  json: "json",
  lock: "json",
  md: "markdown",
  markdown: "markdown",
  nim: "nim",
  nix: "nix",
  proto: "protobuf",
  py: "python",
  rs: "rust",
  sql: "sql",
  ts: "typescript",
  tsx: "typescript",
  html: "xml",
  svg: "xml",
  xml: "xml",
  svelte: "xml",
  yaml: "yaml",
  yml: "yaml",
};

const BY_NAME: Record<string, string> = {
  Dockerfile: "dockerfile",
  Makefile: "makefile",
  "Cargo.lock": "ini",
  "flake.lock": "json",
  ".zshrc": "bash",
  ".bashrc": "bash",
};

/** highlight.js language for a file path, or undefined (plain text). */
export function languageFor(path: string): string | undefined {
  const name = path.split("/").pop() ?? "";
  if (BY_NAME[name]) return BY_NAME[name];
  const ext = name.includes(".") ? name.split(".").pop()!.toLowerCase() : "";
  return BY_EXTENSION[ext];
}

let core: Promise<HLJSApi> | undefined;

async function load(language: string): Promise<HLJSApi | undefined> {
  const loader = LANGUAGES[language];
  if (!loader) return undefined;
  core ??= import("highlight.js/lib/core").then((m) => m.default);
  const hljs = await core;
  if (!hljs.getLanguage(language))
    hljs.registerLanguage(language, (await loader()).default);
  return hljs;
}

/** Highlighted HTML (escaped by highlight.js), or undefined when unsupported. */
export async function highlight(
  code: string,
  language: string,
): Promise<string | undefined> {
  const hljs = await load(language);
  return hljs?.highlight(code, { language, ignoreIllegals: true }).value;
}

/** Highlight `<pre><code class="language-x">` blocks inside rendered Markdown. */
export async function highlightBlocks(root: HTMLElement): Promise<void> {
  const blocks = root.querySelectorAll<HTMLElement>(
    'pre code[class*="language-"]',
  );
  for (const block of blocks) {
    const lang = /language-(\S+)/.exec(block.className)?.[1];
    const name = lang && (LANGUAGES[lang] ? lang : BY_EXTENSION[lang]);
    if (!name) continue;
    const html = await highlight(block.textContent ?? "", name);
    if (html !== undefined) block.innerHTML = html;
  }
}

/** Highlight diff lines with the file grammar; keep the source view's size cap. */
export async function highlightDiff(
  hunks: import("./diff.ts").Hunk[],
  language?: string,
): Promise<(string | undefined)[][] | undefined> {
  if (
    !language ||
    hunks.reduce(
      (size, h) => size + h.lines.reduce((n, l) => n + l.text.length + 1, 0),
      0,
    ) >
      256 * 1024
  )
    return undefined;
  return Promise.all(
    hunks.map((h) =>
      Promise.all(h.lines.map((l) => highlight(l.text, language))),
    ),
  );
}
