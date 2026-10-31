/** The query form of the repository view (ADR 0134) for the old `/tree/…` and `/commits/…` paths. */

/**
 * Splits `<ref>/<path>` (ADR 0021): the longest leading run of segments naming a branch or tag
 * wins; otherwise the first segment is taken as a commit id.
 */
export function splitRefPath(
  segments: string,
  refNames: string[],
): { ref: string; path: string } {
  const parts = segments.split("/").filter(Boolean);
  const short = new Set(
    refNames.map((n) => n.replace(/^refs\/(heads|tags)\//, "")),
  );
  for (let n = parts.length; n > 0; n--) {
    const ref = parts.slice(0, n).join("/");
    if (short.has(ref)) return { ref, path: parts.slice(n).join("/") };
  }
  return { ref: parts[0] ?? "", path: parts.slice(1).join("/") };
}

/** `/<owner>/<name>?ref=…&path=…` for a split `<ref>/<path>`. */
export function repoQuery(key: string, ref: string, path: string): string {
  const q = new URLSearchParams();
  if (ref) q.set("ref", ref);
  if (path) q.set("path", path);
  const s = q.toString();
  return `/${key}${s ? `?${s}` : ""}`;
}
