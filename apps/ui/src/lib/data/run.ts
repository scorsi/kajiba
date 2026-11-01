import { sortChanges } from "../repo-navigation.ts";
/**
 * Everything the run page shows, in one load: the run, its repository and
 * commit, the runs before it on the same ref, the last green run and the
 * commits since, usual step durations, and every step's log.
 */

import {
  ChangeKind,
  canManageRuns,
  getViewer,
  RunState,
  getCommitDiff,
  getRun,
  newRunLog,
  watchRun,
  listCommits,
  listRuns,
  type Change,
  type Commit,
  type Repo,
  type Run,
  type RunLogLine,
  type RunStep,
} from "#lib/api.ts";
import type { RunLog } from "#lib/runlog.ts";
import { allRepos } from "#lib/loaders.ts";

export interface StepLog {
  lines: RunLogLine[];
  complete: boolean;
}

export interface RunPage {
  run: Run;
  canManage: boolean;
  repo?: Repo;
  commit?: Commit;
  /** Files changed since the last green run (or by this commit). */
  changes: Change[];
  /** Runs on the same repository and ref, newest first (includes this one). */
  history: Run[];
  /** Latest succeeded run on the ref before this one. */
  lastGreen?: Run;
  /** Commits after the last green run up to this run's commit, newest first. */
  since: Commit[];
  /** Median duration per step over recent succeeded runs (seconds). */
  usual: number[];
  logs: StepLog[];
  /** The log read so far; `followRun` continues it. */
  log: RunLog<RunStep>;
}

export const terminal = (s: RunState) =>
  s === RunState.SUCCEEDED || s === RunState.FAILED || s === RunState.CANCELLED;

/** The page with its steps and logs rebuilt from `log`; one step per attempt (a replay is a new column). */
function withLog(
  p: Omit<RunPage, "logs" | "log">,
  log: RunLog<RunStep>,
  run: Run,
): RunPage {
  const steps = log.attempts.length
    ? log.attempts.map((a) => a.meta)
    : run.steps;
  const live = !terminal(run.state);
  return {
    ...p,
    run: { ...run, steps },
    log,
    usual: steps.map(() => p.usual[0] ?? 0),
    logs: steps.map((_, i) => ({
      lines: log.attempts.length ? log.lines(i) : [],
      complete: !live || i < steps.length - 1,
    })),
  };
}

/**
 * Follows an unfinished run (ADR 0142): `update` gets the page as log bytes and
 * attempts arrive; resolves with the final run once terminal. Abort `signal`
 * when the page is left.
 */
export async function followRun(
  p: RunPage,
  signal: AbortSignal,
  update: (p: RunPage) => void,
): Promise<Run | undefined> {
  let run = p.run;
  const done = await watchRun({
    id: run.id,
    repo: run.repo,
    log: p.log,
    follow: true,
    signal,
    onChange: () => {
      // The first attempt's log means a runner took the job.
      if (run.state === RunState.QUEUED && p.log.attempts.length)
        run = { ...run, state: RunState.RUNNING };
      update(withLog(p, p.log, run));
    },
  });
  // The final state also settles the last attempt's step.
  if (done) {
    const last = done.steps[0];
    const steps = p.log.attempts.map((a) => a.meta);
    if (last && steps.length) p.log.attempts.at(-1)!.meta = last;
    update(withLog(p, p.log, done));
  }
  return done;
}

const median = (xs: number[]) => {
  if (!xs.length) return 0;
  const s = [...xs].sort((a, b) => a - b);
  return s[Math.floor(s.length / 2)];
};

export async function loadRun(id: string): Promise<RunPage> {
  const run = await getRun({ id });
  const worktree = run.trigger === "worktree";
  const [repos, history, commits, diff, log, viewer] = await Promise.all([
    allRepos(),
    listRuns({ repo: run.repo, ref: run.ref, pageSize: 40 }).then((r) =>
      r.runs.filter((r) =>
        worktree
          ? r.trigger === "worktree"
          : r.trigger !== "worktree" && (!run.ref ? !r.ref : r.ref === run.ref),
      ),
    ),
    // A worktree run's commit is a local one the forge may not have.
    worktree
      ? ([] as Commit[])
      : listCommits({ repo: run.repo, revision: run.commitId, pageSize: 60 })
          .then((r) => r.commits)
          .catch(() => [] as Commit[]),
    worktree
      ? ([] as Change[])
      : getCommitDiff({ repo: run.repo, revision: run.commitId })
          .then((d) => d.changes)
          .catch(() => [] as Change[]),
    readRunLog(run.id, run.repo),
    getViewer(),
  ]);
  const created = (r: Run) => Number(r.createTime?.seconds ?? 0n);
  const lastGreen = history.find(
    (r) =>
      !worktree &&
      r.id !== run.id &&
      created(r) < created(run) &&
      r.state === RunState.SUCCEEDED,
  );
  const greenAt = lastGreen
    ? commits.findIndex((c) => c.id === lastGreen.commitId)
    : -1;
  const since = greenAt > 0 ? commits.slice(0, greenAt) : commits.slice(0, 1);
  // files changed since the last green run (this commit's diff first)
  const diffs = await Promise.all(
    since.slice(1, 12).map((c) =>
      getCommitDiff({ repo: run.repo, revision: c.id })
        .then((d) => d.changes)
        .catch(() => [] as Change[]),
    ),
  );
  const changes = new Map<string, Change>();
  for (const c of [diff, ...diffs].flat())
    if (!changes.has(c.path)) changes.set(c.path, c);
  const green = history
    .filter((r) => r.state === RunState.SUCCEEDED && r.id !== run.id)
    .slice(0, 10);
  return withLog(
    {
      run,
      canManage: canManageRuns(
        repos.find((r) => `${r.owner}/${r.name}` === run.repo),
        viewer,
      ),
      repo: repos.find((r) => `${r.owner}/${r.name}` === run.repo),
      commit: commits[0],
      changes: sortChanges([...changes.values()]),
      history,
      lastGreen,
      since,
      // The browser's median of the ref's recent succeeded runs, for every attempt.
      usual: [median(green.map((r) => r.durationSeconds))],
    },
    log,
    run,
  );
}

async function readRunLog(id: string, repo: string) {
  const log = newRunLog();
  await watchRun({ id, repo, log, follow: false });
  return log;
}

/** A step's log cut into segments: consecutive lines of the same group. */
export interface Segment {
  group?: string;
  /** 1-based line numbers within the step. */
  lines: { n: number; line: RunLogLine }[];
  failed: boolean;
  start: number;
  end: number;
}

export function segments(lines: RunLogLine[], offset = 0): Segment[] {
  const out: Segment[] = [];
  lines.forEach((line, k) => {
    const i = offset + k;
    const last = out.at(-1);
    if (last && last.group === line.group) {
      last.lines.push({ n: i + 1, line });
      last.end = line.offsetSeconds;
      if (line.stream === "err") last.failed = true;
    } else
      out.push({
        group: line.group,
        lines: [{ n: i + 1, line }],
        failed: line.stream === "err",
        start: line.offsetSeconds,
        end: line.offsetSeconds,
      });
  });
  return out;
}

/** Index of the step to look at first: the failing one, else the running one, else the longest. */
export function focusStep(run: Run) {
  const failed = run.steps.findIndex((s) => s.state === RunState.FAILED);
  if (failed >= 0) return failed;
  const running = run.steps.findIndex((s) => s.state === RunState.RUNNING);
  if (running >= 0) return running;
  // all done: the step that took longest (the build, usually)
  return run.steps.reduce(
    (best, x, i) =>
      x.durationSeconds > run.steps[best].durationSeconds ? i : best,
    0,
  );
}

/** Error lines of a step with a little context before each, merged. */
export function excerpt(lines: RunLogLine[], context = 2, max = 14) {
  const keep = new Set<number>();
  lines.forEach((l, i) => {
    if (l.stream !== "err") return;
    for (let k = Math.max(0, i - context); k <= i; k++) keep.add(k);
  });
  return [...keep]
    .sort((a, b) => a - b)
    .slice(-max)
    .map((i) => ({ n: i + 1, line: lines[i] }));
}

export const runHref = (id: string, extra: Record<string, string> = {}) => {
  const q = new URLSearchParams(extra).toString();
  return `/-/runs/${id}${q ? `?${q}` : ""}`;
};

export const clock = (s: number) => {
  const t = Math.max(0, Math.round(s));
  return `${Math.floor(t / 60)}:${String(t % 60).padStart(2, "0")}`;
};

export const CHANGE_LABEL: Record<ChangeKind, string> = {
  [ChangeKind.UNSPECIFIED]: "changed",
  [ChangeKind.ADDED]: "added",
  [ChangeKind.MODIFIED]: "modified",
  [ChangeKind.DELETED]: "deleted",
};
