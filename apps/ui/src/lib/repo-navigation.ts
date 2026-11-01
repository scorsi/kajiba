/** Keep the history scope and ribbon day stable while browsing a commit's files. */
export function repoQuery(
  q: Pick<URLSearchParams, "get" | "toString">,
  next: Record<string, string | null>,
) {
  const u = new URLSearchParams(q.toString());
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
