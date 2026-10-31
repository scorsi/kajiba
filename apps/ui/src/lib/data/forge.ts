/**
 * Forge-wide data for Activity and Administration: every repository with its
 * recent commits (default branch), releases, CI runs, mirror state, runners.
 * Composed from per-repository RPCs; a real page would want one
 * `GetActivity`-style RPC instead (see DESIGN.md).
 */

import {
  getViewer,
  listCommits,
  listMirrorStatus,
  listRefs,
  listRunners,
  type Commit,
  type MirrorStatus,
  type Repo,
  type Run,
  type Runner,
} from "#lib/api.ts";
import { refShortName, toDate } from "#lib/format.ts";
import { allRepos, recentRuns } from "#lib/loaders.ts";

export interface RepoActivity {
  repo: Repo;
  key: string;
  commits: Commit[];
  /** commit id → release names */
  tags: Map<string, string[]>;
  runs: Run[];
  mirror?: MirrorStatus;
}

export interface Forge {
  repos: RepoActivity[];
  runs: Run[];
  runners: Runner[];
  mirrors: MirrorStatus[];
}

export async function loadForge(): Promise<Forge> {
  // Runners and mirrors are for administrators (ADR 0139).
  const { admin } = await getViewer();
  const [repos, runs, runners, mirrors] = await Promise.all([
    allRepos(),
    recentRuns(50),
    admin ? listRunners().then((r) => r.runners) : [],
    admin ? listMirrorStatus().then((m) => m.mirrors) : [],
  ]);
  const per = await Promise.all(
    repos.map(async (repo) => {
      const key = `${repo.owner}/${repo.name}`;
      const [commits, refs] = await Promise.all([
        listCommits({ repo: key, revision: repo.defaultBranch, want: 300 })
          .then((l) => l.commits)
          .catch(() => [] as Commit[]),
        listRefs({ repo: key }).then((r) => r.refs),
      ]);
      const tags = new Map<string, string[]>();
      for (const r of refs)
        if (r.name.startsWith("refs/tags/"))
          tags.set(r.commitId, [
            ...(tags.get(r.commitId) ?? []),
            refShortName(r.name),
          ]);
      return {
        repo,
        key,
        commits,
        tags,
        runs: runs.filter((r) => r.repo === key),
        mirror: mirrors.find((m) => m.repo === key),
      };
    }),
  );
  return { repos: per, runs, runners, mirrors };
}

/** Local calendar day key. */
export const dayKey = (dt: Date) =>
  `${dt.getFullYear()}-${String(dt.getMonth() + 1).padStart(2, "0")}-${String(dt.getDate()).padStart(2, "0")}`;

/** The last `n` days, oldest first. */
export function lastDays(n: number, now = new Date()) {
  return Array.from({ length: n }, (_, i) => {
    const dt = new Date(now.getTime() - (n - 1 - i) * 86400000);
    return { k: dayKey(dt), dt };
  });
}

export const commitDay = (c: Commit) => dayKey(toDate(c.commitTime)!);

export const dur = (s: number) =>
  s >= 60
    ? `${Math.floor(s / 60)}m${String(s % 60).padStart(2, "0")}s`
    : `${s}s`;

/** Link into the repository view: `/<owner>/<name>?<view state>`. */
export function repoHref(key: string, extra: Record<string, string> = {}) {
  const q = new URLSearchParams(extra).toString();
  return `/${key}${q ? `?${q}` : ""}`;
}
