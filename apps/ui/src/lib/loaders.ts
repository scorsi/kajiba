/**
 * Page-level data composition on top of `api.ts`. Views call these; they only
 * combine RPCs, so they survive the swap to the real client unchanged.
 */

import * as api from "./api";

/** Every repository the viewer can read, paged from `ListRepos` and cached for the session. */
let repoCache: Promise<api.Repo[]> | undefined;

export function allRepos(): Promise<api.Repo[]> {
  repoCache ??= (async () => {
    const out: api.Repo[] = [];
    let pageToken = "";
    do {
      const res = await api.listRepos({ pageSize: 100, pageToken });
      out.push(...res.repos);
      pageToken = res.nextPageToken;
    } while (pageToken);
    return out.sort((a, b) =>
      `${a.owner}/${a.name}`.localeCompare(`${b.owner}/${b.name}`),
    );
  })();
  repoCache.catch(() => (repoCache = undefined));
  return repoCache;
}

/** `GetRepo` resolves previous names: the view redirects to the current one. */
export async function findRepo(owner: string, name: string) {
  const repo = await api.getRepo({ name: `${owner}/${name}` });
  const renamed = repo.owner !== owner || repo.name !== name;
  return { repo, renamedFrom: renamed ? name : undefined };
}

/** Directories first, then files, each alphabetical (case-insensitive). */
export function sortEntries(entries: api.TreeEntry[]): api.TreeEntry[] {
  const rank = (e: api.TreeEntry) => (e.kind === api.EntryKind.TREE ? 0 : 1);
  return [...entries].sort(
    (a, b) =>
      rank(a) - rank(b) ||
      a.name.localeCompare(b.name, "en", { sensitivity: "base" }),
  );
}
