/** Calendar buckets shared by forge Activity and repository Pulse (ADR 0154). */
import { RunState, type Commit, type Run } from "#lib/api.ts";
import { dayKey, lastDays, uniqueCommits } from "#lib/data/forge.ts";

export const RANGES = [
  { days: 7, label: "7 days" },
  { days: 30, label: "30 days" },
  { days: 365, label: "1 year" },
] as const;
export const activityRange = (value: string | null) =>
  value === "7" ? 7 : value === "365" ? 365 : 30;
export interface ActivitySource {
  repo: string;
  ref: string;
  commits: Commit[];
  tags: Map<string, string[]>;
  runs: Run[];
}
export interface Push {
  repo: string;
  ref: string;
  author: string;
  commits: Commit[];
}
export const OUTCOMES = [
  RunState.SUCCEEDED,
  RunState.FAILED,
  RunState.CANCELLED,
  RunState.RUNNING,
  RunState.QUEUED,
];
export const outcomeCounts = (runs: Run[]) =>
  OUTCOMES.map((state) => runs.filter((r) => r.state === state).length);
const date = (t?: { seconds: bigint }) =>
  new Date(Number(t?.seconds ?? 0n) * 1000);
const hourKey = (dt: Date) =>
  `${dayKey(dt)}T${String(dt.getHours()).padStart(2, "0")}`;
const monday = (dt: Date) =>
  new Date(
    dt.getFullYear(),
    dt.getMonth(),
    dt.getDate() - ((dt.getDay() + 6) % 7),
  );
const weekKey = (dt: Date) => `week-${dayKey(monday(dt))}`;
export const bucketKey = (dt: Date, days: number) =>
  days === 7 ? hourKey(dt) : days === 30 ? dayKey(dt) : weekKey(dt);

/** The first included local calendar day; clamp February 29 in the prior year. */
export function activityStart(days: number, now = new Date()) {
  if (days !== 365)
    return new Date(
      now.getFullYear(),
      now.getMonth(),
      now.getDate() - days + 1,
    );
  const year = now.getFullYear() - 1;
  const month = now.getMonth();
  const day = Math.min(now.getDate(), new Date(year, month + 1, 0).getDate());
  return new Date(year, month, day + 1);
}

export function activityBuckets(
  sources: ActivitySource[],
  days: number,
  now = new Date(),
) {
  const start = activityStart(days, now);
  const end = new Date(now.getFullYear(), now.getMonth(), now.getDate() + 1);
  const slots: { k: string; dt: Date; label: string }[] = [];
  if (days === 365) {
    for (
      let dt = monday(start);
      dt < end;
      dt = new Date(dt.getFullYear(), dt.getMonth(), dt.getDate() + 7)
    ) {
      slots.push({ k: weekKey(dt), dt, label: `Week of ${dayKey(dt)}` });
    }
  } else {
    for (const { dt, k } of lastDays(days, now)) {
      if (days === 7) {
        // A fixed wall-clock card merges the repeated DST hour; a skipped hour stays empty.
        for (let hour = 0; hour < 24; hour++) {
          const key = `${k}T${String(hour).padStart(2, "0")}`;
          slots.push({
            k: key,
            dt,
            label: `${k} ${String(hour).padStart(2, "0")}:00`,
          });
        }
      } else slots.push({ k, dt, label: k });
    }
  }
  const buckets = slots.map((slot) => ({
    ...slot,
    pushes: [] as Push[],
    runs: [] as Run[],
    releases: [] as { repo: string; name: string; commit: Commit }[],
  }));
  const byKey = new Map(buckets.map((b) => [b.k, b]));
  const inRange = (dt: Date) => dt >= start && dt < end;
  for (const src of sources) {
    const groups = new Map<string, Push>();
    for (const commit of uniqueCommits(src.commits)) {
      if (!commit.commitTime) continue;
      const dt = date(commit.commitTime);
      if (!inRange(dt)) continue;
      const bucket = byKey.get(bucketKey(dt, days));
      if (!bucket) continue;
      // shortcut: infer pushes from author/repository/hour; use real events if a push API is added.
      const key = JSON.stringify([
        hourKey(dt),
        commit.authorName,
        commit.authorEmail,
      ]);
      let push = groups.get(key);
      if (!push) {
        push = {
          repo: src.repo,
          ref: src.ref,
          author: commit.authorName,
          commits: [],
        };
        groups.set(key, push);
        bucket.pushes.push(push);
      }
      push.commits.push(commit);
      for (const name of src.tags.get(commit.id) ?? [])
        bucket.releases.push({ repo: src.repo, name, commit });
    }
    const seen = new Set<string>();
    for (const run of src.runs) {
      if (seen.has(run.id) || !run.createTime) continue;
      seen.add(run.id);
      const dt = date(run.createTime);
      if (inRange(dt)) byKey.get(bucketKey(dt, days))?.runs.push(run);
    }
  }
  return buckets;
}
export type ActivityBucket = ReturnType<typeof activityBuckets>[number];

export function aggregate(bucket: ActivityBucket, byRef = false) {
  const groups = new Map<string, { commits: number; runs: Run[] }>();
  const group = (key: string) => {
    if (!groups.has(key)) groups.set(key, { commits: 0, runs: [] });
    return groups.get(key)!;
  };
  for (const push of bucket.pushes)
    group(byRef ? push.ref : push.repo).commits += push.commits.length;
  for (const run of bucket.runs)
    group(
      byRef
        ? run.trigger === "worktree"
          ? "worktree"
          : run.ref || "manual (no ref)"
        : run.repo,
    ).runs.push(run);
  const durations = bucket.runs
    .filter(
      (r) =>
        r.state === RunState.SUCCEEDED ||
        r.state === RunState.FAILED ||
        r.state === RunState.CANCELLED,
    )
    .map((r) => r.durationSeconds)
    .sort((a, b) => a - b);
  const mid = Math.floor(durations.length / 2);
  return {
    groups: [...groups].map(([name, value]) => ({
      name,
      commits: value.commits,
      runs: value.runs.length,
      outcomes: outcomeCounts(value.runs),
    })),
    median: durations.length
      ? (durations[mid] + durations[Math.floor((durations.length - 1) / 2)]) / 2
      : undefined,
    longest: durations.at(-1),
    branch: outcomeCounts(bucket.runs.filter((r) => r.trigger !== "worktree")),
    worktree: outcomeCounts(
      bucket.runs.filter((r) => r.trigger === "worktree"),
    ),
  };
}

/** Calendar-based lookback includes the six days before the first visible bar. */
export function successTrend(sources: ActivitySource[], now = new Date()) {
  const daily = activityBuckets(sources, 30, now);
  const runs = sources
    .flatMap((s) => s.runs)
    .filter(
      (r) =>
        r.trigger !== "worktree" &&
        (r.state === RunState.SUCCEEDED || r.state === RunState.FAILED),
    );
  return daily.map((b) => {
    const start = new Date(
      b.dt.getFullYear(),
      b.dt.getMonth(),
      b.dt.getDate() - 6,
    );
    const end = new Date(
      b.dt.getFullYear(),
      b.dt.getMonth(),
      b.dt.getDate() + 1,
    );
    const window = runs.filter(
      (r) =>
        r.createTime && date(r.createTime) >= start && date(r.createTime) < end,
    );
    return window.length
      ? window.filter((r) => r.state === RunState.SUCCEEDED).length /
          window.length
      : null;
  });
}
