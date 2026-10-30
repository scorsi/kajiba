/**
 * Page-level data composition on top of `api.ts`. Views call these; they only
 * combine RPCs, so they survive the swap to the real client unchanged.
 */

import * as api from "./api";
import { README_NAMES } from "./format";

/**
 * There is no `GetRepo` RPC: find a repository by paging `ListRepos`
 * (a few dozen repos, cached for the session). Old names resolve to the
 * current one so the view can redirect.
 */
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

export async function findRepo(owner: string, name: string) {
  const repos = await allRepos();
  const exact = repos.find((r) => r.owner === owner && r.name === name);
  if (exact) return { repo: exact, renamedFrom: undefined };
  const moved = repos.find(
    (r) => r.owner === owner && r.previousNames.includes(name),
  );
  if (moved) return { repo: moved, renamedFrom: name };
  throw new api.ApiError(
    api.Code.NotFound,
    `repository ${owner}/${name} not found`,
  );
}

export interface DirectoryView {
  commitId: string;
  entries: api.TreeEntry[];
  /** Latest commit of the revision (ListCommits has no path filter). */
  lastCommit?: api.Commit;
  readme?: { name: string; source: string };
}

/** Directory listing + README + latest commit, for the repo home and tree pages. */
export async function loadDirectory(
  key: string,
  revision: string,
  path = "",
): Promise<DirectoryView> {
  const [tree, log] = await Promise.all([
    api.getTree({ repo: key, revision, path }),
    api.listCommits({ repo: key, revision, pageSize: 1 }),
  ]);
  const readmeEntry = README_NAMES.map((n) =>
    tree.entries.find((e) => e.name === n && e.kind === api.EntryKind.BLOB),
  ).find(Boolean);
  let readme: DirectoryView["readme"];
  if (readmeEntry) {
    const blob = await api.getBlob({
      repo: key,
      revision,
      path: path ? `${path}/${readmeEntry.name}` : readmeEntry.name,
    });
    readme = {
      name: readmeEntry.name,
      source: new TextDecoder().decode(blob.content),
    };
  }
  return {
    commitId: tree.commitId,
    entries: sortEntries(tree.entries),
    lastCommit: log.commits[0],
    readme,
  };
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
