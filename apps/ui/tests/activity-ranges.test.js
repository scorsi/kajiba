import assert from "node:assert/strict";
import { test } from "node:test";
import { createServer } from "vite";

// Run in an explicit viewer zone, independent of the test runner's host zone.
process.env.TZ = "Europe/Paris";
const timestamp = (value) => ({
  seconds: BigInt(new Date(value).getTime() / 1000),
  nanos: 0,
});
const commit = (id, value, author = "alice") => ({
  id,
  authorName: author,
  authorEmail: `${author}@example.org`,
  commitTime: timestamp(value),
  message: id,
});
const source = (over = {}) => ({
  repo: "alice/demo",
  ref: "refs/heads/main",
  commits: [],
  tags: new Map(),
  runs: [],
  ...over,
});
async function modules(t) {
  const vite = await createServer({
    configFile: false,
    server: { middlewareMode: true, ws: false },
  });
  t.after(() => vite.close());
  return {
    ...(await vite.ssrLoadModule("/src/lib/data/activity.ts")),
    ...(await vite.ssrLoadModule("/src/lib/api.ts")),
  };
}

test("7-day wall-clock card has unique cells, merges repeated hours and leaves skipped hours empty", async (t) => {
  const { activityBuckets, RunState } = await modules(t);
  const fall = source({
    runs: [
      {
        id: "hour-run",
        repo: "alice/demo",
        state: RunState.FAILED,
        createTime: timestamp("2026-10-25T02:45:00+01:00"),
      },
    ],
    commits: [
      commit("first", "2026-10-25T02:20:00+02:00"),
      commit("second", "2026-10-25T02:20:00+01:00"),
      commit("other", "2026-10-25T03:20:00+01:00"),
    ],
  });
  const buckets = activityBuckets(
    [fall],
    7,
    new Date("2026-10-26T10:00:00+01:00"),
  );
  assert.equal(buckets.length, 7 * 24);
  assert.equal(new Set(buckets.map((b) => b.k)).size, 168);
  assert.equal(
    buckets.find((b) => b.k === "2026-10-25T02").pushes[0].commits.length,
    2,
  );
  assert.equal(buckets.find((b) => b.k === "2026-10-25T03").pushes.length, 1);
  assert.equal(
    buckets.find((b) => b.k === "2026-10-25T02").runs[0].state,
    RunState.FAILED,
  );
  assert.equal(
    buckets.reduce((n, b) => n + b.runs.length, 0),
    1,
  );
  const spring = activityBuckets(
    [
      source({
        commits: [
          commit("a", "2026-03-29T01:30:00+01:00"),
          commit("b", "2026-03-29T03:30:00+02:00"),
        ],
      }),
    ],
    7,
    new Date("2026-03-30T12:00:00+02:00"),
  );
  assert.equal(spring.find((b) => b.k === "2026-03-29T02").pushes.length, 0);
  assert.equal(spring.find((b) => b.k === "2026-03-29T03").pushes.length, 1);
});

test("calendar days and Monday weeks include both DST transitions, year boundaries and leap years", async (t) => {
  const { activityBuckets, activityStart } = await modules(t);
  assert.equal(
    activityStart(365, new Date(2024, 1, 29)).toLocaleDateString("sv"),
    "2023-03-01",
  );
  for (const now of [
    new Date(2026, 2, 30),
    new Date(2026, 9, 26),
    new Date(2027, 0, 1),
    new Date(2024, 2, 1),
  ]) {
    const daily = activityBuckets([], 30, now);
    assert.equal(daily.length, 30);
    assert.equal(new Set(daily.map((b) => b.k)).size, 30);
    const weeks = activityBuckets([], 365, now);
    assert.ok(weeks.length >= 53 && weeks.length <= 54);
    assert.equal(new Set(weeks.map((b) => b.k)).size, weeks.length);
    for (let i = 1; i < weeks.length; i++) {
      const previous = weeks[i - 1].dt;
      assert.equal(
        weeks[i].dt.getTime(),
        new Date(
          previous.getFullYear(),
          previous.getMonth(),
          previous.getDate() + 7,
        ).getTime(),
      );
      assert.equal(weeks[i].dt.getDay(), 1);
    }
  }
  const weeks = activityBuckets(
    [
      source({
        commits: [
          commit("spring", "2026-03-29T23:59:00+02:00"),
          commit("monday", "2026-03-30T00:00:00+02:00"),
          commit("fall", "2026-10-25T23:59:00+01:00"),
        ],
      }),
    ],
    365,
    new Date(2026, 9, 26),
  );
  assert.equal(weeks.find((b) => b.k === "week-2026-03-23").pushes.length, 1);
  assert.equal(weeks.find((b) => b.k === "week-2026-03-30").pushes.length, 1);
  assert.equal(weeks.find((b) => b.k === "week-2026-10-19").pushes.length, 1);
  const weekHours = (key) => {
    const i = weeks.findIndex((b) => b.k === key);
    return (weeks[i + 1].dt - weeks[i].dt) / 3600000;
  };
  assert.equal(weekHours("week-2026-03-23"), 167);
  assert.equal(weekHours("week-2026-10-19"), 169);
  const bounds = activityBuckets(
    [
      source({
        commits: [
          commit("old", "2025-11-01T23:59:00+01:00"),
          commit("start", "2025-11-02T00:00:00+01:00"),
          commit("end", "2026-11-01T23:59:00+01:00"),
          commit("future", "2026-11-02T00:00:00+01:00"),
        ],
      }),
    ],
    365,
    new Date(2026, 10, 1),
  );
  assert.deepEqual(
    bounds.flatMap((b) => b.pushes.flatMap((p) => p.commits.map((c) => c.id))),
    ["start", "end"],
  );
});

test("daily aggregates count every outcome, separate worktrees, group by repository or ref, and compute durations", async (t) => {
  const { activityBuckets, aggregate, RunState } = await modules(t);
  const at = "2026-10-25T10:00:00+01:00";
  const makeRun = (id, state, durationSeconds, over = {}) => ({
    id,
    repo: "alice/demo",
    ref: "refs/heads/main",
    state,
    durationSeconds,
    createTime: timestamp(at),
    ...over,
  });
  const runs = [
    makeRun("a", RunState.SUCCEEDED, 10),
    makeRun("b", RunState.FAILED, 20),
    makeRun("c", RunState.CANCELLED, 40),
    makeRun("d", RunState.RUNNING, 999),
    makeRun("e", RunState.QUEUED, 0),
    makeRun("f", RunState.SUCCEEDED, 30, { trigger: "worktree", ref: "" }),
    makeRun("g", RunState.FAILED, 100, { ref: "refs/heads/feature" }),
  ];
  const c = commit("one", at);
  const src = source({
    commits: [c, c, commit("two", at), commit("three", at, "bob")],
    tags: new Map([["one", ["v1"]]]),
    runs: [...runs, runs[0]],
  });
  const other = source({
    repo: "bob/other",
    commits: [c],
    runs: [makeRun("h", RunState.SUCCEEDED, 60, { repo: "bob/other" })],
  });
  const buckets = activityBuckets([src, other], 30, new Date(2026, 9, 26));
  const day = buckets.find((b) => b.k === "2026-10-25");
  assert.equal(day.pushes.length, 3);
  assert.equal(
    day.pushes.reduce((n, p) => n + p.commits.length, 0),
    4,
  );
  assert.equal(day.releases.length, 1);
  assert.equal(day.runs.length, 8);
  const a = aggregate(day);
  assert.deepEqual(a.branch, [2, 2, 1, 1, 1]);
  assert.deepEqual(a.worktree, [1, 0, 0, 0, 0]);
  assert.equal(a.median, 35);
  assert.equal(a.longest, 100);
  assert.deepEqual(
    a.groups.map((g) => [g.name, g.commits, g.runs]),
    [
      ["alice/demo", 3, 7],
      ["bob/other", 1, 1],
    ],
  );
  assert.deepEqual(
    aggregate(day, true).groups.map((g) => [g.name, g.commits, g.runs]),
    [
      ["refs/heads/main", 4, 6],
      ["worktree", 0, 1],
      ["refs/heads/feature", 0, 1],
    ],
  );
  assert.equal(aggregate(buckets[0]).median, undefined);
  const one = { ...day, runs: runs.slice(0, 3) };
  assert.equal(aggregate(one).median, 20);
});

test("rolling success includes the six prior calendar days and excludes worktree, cancelled and active runs", async (t) => {
  const { successTrend, RunState, activityRange } = await modules(t);
  const run = (state, value, trigger) => ({
    state,
    createTime: timestamp(value),
    trigger,
  });
  const trend = successTrend(
    [
      source({
        runs: [
          run(RunState.SUCCEEDED, "2026-09-25T00:00:00+02:00"),
          run(RunState.FAILED, "2026-10-01T00:00:00+02:00"),
          run(RunState.CANCELLED, "2026-10-01T00:00:00+02:00"),
          run(RunState.SUCCEEDED, "2026-10-01T00:00:00+02:00", "worktree"),
        ],
      }),
    ],
    new Date(2026, 9, 30),
  );
  assert.equal(trend[0], 0.5);
  assert.equal(trend[1], 0);
  assert.equal(trend[7], null);
  assert.deepEqual(
    ["7", "30", "365", "1", "90", null].map(activityRange),
    [7, 30, 365, 30, 30, 30],
  );
});
