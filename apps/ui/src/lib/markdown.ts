/**
 * README rendering: Markdown → sanitised HTML.
 *
 * `marked` (GFM: tables, task lists) + `DOMPurify` (README content is
 * untrusted). Both are loaded lazily so they stay out of the initial bundle.
 */

export interface RenderOptions {
  /** Base used to resolve relative links, e.g. `/scorsi/kajiba/tree/main/docs/`. */
  linkBase?: string;
}

export async function renderMarkdown(
  source: string,
  opts: RenderOptions = {},
): Promise<string> {
  const [{ Marked }, { default: DOMPurify }] = await Promise.all([
    import("marked"),
    import("dompurify"),
  ]);
  const marked = new Marked({ gfm: true, breaks: false });
  const raw = await marked.parse(source);
  const clean = DOMPurify.sanitize(raw, { USE_PROFILES: { html: true } });
  return postProcess(clean, opts.linkBase);
}

/**
 * - relative links (`docs/adr`) point into the repository browser;
 * - task-list checkboxes get an accessible name;
 * - scrollable code blocks are keyboard-focusable.
 */
function postProcess(html: string, base?: string): string {
  const doc = new DOMParser().parseFromString(html, "text/html");
  for (const box of doc.querySelectorAll<HTMLInputElement>(
    'input[type="checkbox"]',
  )) {
    box.setAttribute(
      "aria-label",
      box.hasAttribute("checked") ? "done" : "to do",
    );
  }
  for (const pre of doc.querySelectorAll("pre"))
    pre.setAttribute("tabindex", "0");
  for (const a of doc.querySelectorAll<HTMLAnchorElement>("a[href]")) {
    const href = a.getAttribute("href")!;
    if (/^([a-z][a-z0-9+.-]*:|#|\/)/i.test(href)) {
      if (/^https?:/i.test(href)) a.setAttribute("rel", "noopener noreferrer");
      continue;
    }
    if (base) a.setAttribute("href", base + href.replace(/^\.\//, ""));
  }
  return doc.body.innerHTML;
}
