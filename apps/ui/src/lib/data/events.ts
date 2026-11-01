/**
 * History events: what happened, at forge or repository scope.
 *
 * Commits are grouped per (day, author, repository); releases, failed and active runs,
 * failing mirrors and offline runners are events of their own. Everything is
 * derived client-side from the RPCs we already call; a real server would want
 * one `ListEvents`-style RPC instead (see DESIGN.md, open questions).
 */

import {
  MirrorState,
  RunState,
  RunnerState,
  type Commit,
  type MirrorStatus,
  type Run,
  type Runner,
} from "#lib/api.ts";
import { dayKey, uniqueCommits, type Forge } from "#lib/data/forge.ts";
import { refShortName } from "#lib/format.ts";

export type HistoryEvent =
  | {
      t: number;
      kind: "commits";
      repo: string;
      author: string;
      commits: Commit[];
    }
  | { t: number; kind: "release"; repo: string; name: string; commit: Commit }
  | { t: number; kind: "ci"; repo: string; run: Run; fixed: boolean }
  | { t: number; kind: "mirror"; repo: string; mirror: MirrorStatus }
  | { t: number; kind: "runner"; runner: Runner };

export type EventKind = HistoryEvent["kind"];

/** One repository's raw material. */
export interface EventSource {
  repo: string;
  /** Newest first (as ListCommits returns them). */
  commits: Commit[];
  /** commit id → release names */
  tags: Map<string, string[]>;
  runs: Run[];
}

export interface BuildOptions {
  mirrors?: MirrorStatus[];
  runners?: Runner[];
  /** Unix seconds; commits, releases and runs before this are left out. */
  since?: number;
  /** Only keep runs whose commit is among the source commits (path scope). */
  runsOnListedCommits?: boolean;
}

const secs = (t?: { seconds: bigint }) => Number(t?.seconds ?? 0n);

/** Recovery is scoped to the exact repository/ref, never to local sources. */
export function isFixed(run: Run, runs: Run[]) {
  return (
    run.state === RunState.FAILED &&
    run.trigger !== "worktree" &&
    !!run.createTime &&
    runs.some(
      (next) =>
        next.trigger !== "worktree" &&
        next.state === RunState.SUCCEEDED &&
        next.repo === run.repo &&
        next.ref === run.ref &&
        !!next.createTime &&
        (next.createTime.seconds > run.createTime!.seconds ||
          (next.createTime.seconds === run.createTime!.seconds &&
            (next.createTime.nanos ?? 0) > (run.createTime!.nanos ?? 0))),
    )
  );
}

export function buildEvents(
  sources: EventSource[],
  opts: BuildOptions = {},
): HistoryEvent[] {
  const since = opts.since ?? 0;
  const out: HistoryEvent[] = [];
  for (const src of sources) {
    const s = { ...src, commits: uniqueCommits(src.commits) };
    const groups = new Map<string, Commit[]>();
    const listed = new Set<string>();
    for (const c of s.commits) {
      const t = secs(c.commitTime);
      if (t < since) break;
      listed.add(c.id);
      const g = `${dayKey(new Date(t * 1000))}|${c.authorName}`;
      const group = groups.get(g);
      if (group) group.push(c);
      else groups.set(g, [c]);
      // a release sorts just above the commit it points at
      for (const name of s.tags.get(c.id) ?? [])
        out.push({ t: t + 1, kind: "release", repo: s.repo, name, commit: c });
    }
    for (const commits of groups.values())
      out.push({
        t: secs(commits[0].commitTime),
        kind: "commits",
        repo: s.repo,
        author: commits[0].authorName,
        commits,
      });
    for (const run of s.runs) {
      if (
        ![RunState.FAILED, RunState.RUNNING, RunState.QUEUED].includes(
          run.state,
        )
      )
        continue;
      if (secs(run.createTime) < since) continue;
      if (opts.runsOnListedCommits && !listed.has(run.commitId)) continue;
      out.push({
        t:
          secs(run.createTime) +
          (run.state === RunState.FAILED ? run.durationSeconds : 0),
        kind: "ci",
        repo: s.repo,
        run,
        fixed: isFixed(run, s.runs),
      });
    }
  }
  // Current states: shown whatever the period, at the time they started.
  for (const m of opts.mirrors ?? [])
    if (m.state === MirrorState.FAILING)
      out.push({
        t: secs(m.lastAttemptTime),
        kind: "mirror",
        repo: m.repo,
        mirror: m,
      });
  for (const r of opts.runners ?? [])
    if (r.state === RunnerState.OFFLINE && !r.disabled)
      out.push({ t: secs(r.lastSeenTime), kind: "runner", runner: r });
  return out.sort((a, b) => b.t - a.t);
}

/** Filter chips. `ops` covers mirrors and runners. */
export const EVENT_FILTERS = [
  { id: "all", label: "All" },
  { id: "commits", label: "Commits" },
  { id: "release", label: "Releases" },
  { id: "ci", label: "CI" },
  { id: "ops", label: "Mirrors & runners" },
] as const;
export type EventFilter = (typeof EVENT_FILTERS)[number]["id"];

export const parseFilter = (v: string | null): EventFilter =>
  EVENT_FILTERS.some((f) => f.id === v) ? (v as EventFilter) : "all";

export function matches(e: HistoryEvent, filter: EventFilter, repo = "") {
  const kindOk =
    filter === "all" ||
    e.kind === filter ||
    (filter === "ops" && (e.kind === "mirror" || e.kind === "runner"));
  return kindOk && (!repo || ("repo" in e && e.repo === repo));
}

/** Counts for the folded strip and the chips. */
export function countEvents(events: HistoryEvent[]) {
  const n = { commits: 0, release: 0, ci: 0, mirror: 0, runner: 0 };
  for (const e of events)
    n[e.kind] += e.kind === "commits" ? e.commits.length : 1;
  return n;
}

/** Default-branch failures and distinct active runs; worktrees never count. */
export function attention(forge: Forge, hideFixed = true) {
  return {
    branches: forge.repos.flatMap((r) => {
      const tip = r.commits[0]?.id;
      const last = [...r.runs]
        .sort(
          (a, b) =>
            secs(b.createTime) - secs(a.createTime) ||
            (b.createTime?.nanos ?? 0) - (a.createTime?.nanos ?? 0),
        )
        .find(
          (x) =>
            x.trigger !== "worktree" &&
            x.state === RunState.FAILED &&
            (x.ref === r.repo.defaultBranch || (!x.ref && x.commitId === tip)),
        );
      const fixed = last ? isFixed(last, r.runs) : false;
      return last && (!hideFixed || !fixed)
        ? [
            {
              key: r.key,
              branch: refShortName(r.repo.defaultBranch),
              run: last,
              fixed,
            },
          ]
        : [];
    }),
    active: forge.runs.filter(
      (r) =>
        r.trigger !== "worktree" &&
        (r.state === RunState.QUEUED || r.state === RunState.RUNNING),
    ),
    mirrors: forge.mirrors.filter((m) => m.state === MirrorState.FAILING),
    runners: forge.runners.filter(
      (r) => r.state === RunnerState.OFFLINE && !r.disabled,
    ),
  };
}
