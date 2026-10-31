/**
 * History events: what happened, at forge or repository scope.
 *
 * Commits are grouped per (day, author, repository); releases, failed runs,
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
import { dayKey } from "#lib/data/forge.ts";

export type HistoryEvent =
  | {
      t: number;
      kind: "commits";
      repo: string;
      author: string;
      commits: Commit[];
    }
  | { t: number; kind: "release"; repo: string; name: string; commit: Commit }
  | { t: number; kind: "ci"; repo: string; run: Run }
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

export function buildEvents(
  sources: EventSource[],
  opts: BuildOptions = {},
): HistoryEvent[] {
  const since = opts.since ?? 0;
  const out: HistoryEvent[] = [];
  for (const s of sources) {
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
      if (run.state !== RunState.FAILED) continue;
      if (secs(run.createTime) < since) continue;
      if (opts.runsOnListedCommits && !listed.has(run.commitId)) continue;
      out.push({
        t: secs(run.createTime) + run.durationSeconds,
        kind: "ci",
        repo: s.repo,
        run,
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
    if (r.state === RunnerState.OFFLINE)
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
