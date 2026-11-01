/** Keep the history scope and ribbon day stable while browsing a commit's files. */
export function repoQuery(
  q: Pick<URLSearchParams, "get" | "toString">,
  next: Record<string, string | null>,
) {
  const u = new URLSearchParams(q.toString());
  // Every navigation chooses its mode, even when it targets the current path.
  if ("path" in next || "at" in next || "ref" in next) u.delete("view");
  if (next.at && !q.get("at")) u.set("scope", q.get("path") ?? "");
  if (next.at === null) {
    const scope = q.get("scope") ?? q.get("path") ?? "";
    if (scope) u.set("path", scope);
    else u.delete("path");
    u.delete("scope");
  }
  for (const [key, value] of Object.entries(next)) {
    if (value === null || value === "") u.delete(key);
    else u.set(key, value);
  }
  return `?${u}`;
}

export const isMarkdown = (path: string) => /\.(md|markdown)$/i.test(path);

/** Deterministic alphabetical path order, independent of the browser's locale. */
export function sortChanges<T extends { path: string }>(changes: T[]): T[] {
  return [...changes].sort((a, b) =>
    a.path < b.path ? -1 : a.path > b.path ? 1 : 0,
  );
}

export function localDateTime(date: Date | undefined): string {
  return (
    date?.toLocaleString(undefined, {
      year: "numeric",
      month: "2-digit",
      day: "2-digit",
      hour: "2-digit",
      minute: "2-digit",
      second: "2-digit",
      timeZoneName: "short",
    }) ?? ""
  );
}

export const fileViewMode = (q: Pick<URLSearchParams, "get">) =>
  q.get("view") === "diff" ? "diff" : "file";

/** Changed files and each ancestor, using exact paths rather than prefixes. */
export function touchedPaths(
  changes: readonly { path: string }[],
): Set<string> {
  const paths = new Set<string>();
  for (const { path } of changes) {
    const parts = path.split("/");
    for (let i = 1; i <= parts.length; i++)
      paths.add(parts.slice(0, i).join("/"));
  }
  return paths;
}
