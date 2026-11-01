import assert from "node:assert/strict";
import { test } from "node:test";
import { createServer } from "vite";

const at = (day, hour = 12) => ({
  seconds: BigInt(Date.UTC(2026, 9, day, hour) / 1000),
});
const commit = (id, author, day, hour) => ({
  id,
  authorName: author,
  commitTime: at(day, hour),
});
const run = (id, state, over = {}) => ({
  id,
  state,
  ref: "",
  commitId: "",
  createTime: at(20),
  durationSeconds: 5,
  ...over,
});

test("events group commits per day and author; attention follows the default branch", async (t) => {
  const vite = await createServer({
    configFile: false,
    server: { middlewareMode: true, ws: false },
  });
  t.after(() => vite.close());
  const { buildEvents, attention, countEvents } = await vite.ssrLoadModule(
    "/src/lib/data/events.ts",
  );
  const { RunState } = await vite.ssrLoadModule("/src/lib/api.ts");

  const commits = [
    commit("c4", "bob", 20, 10),
    commit("c3", "alice", 20, 9),
    commit("c2", "alice", 20, 8),
    commit("c1", "alice", 3, 8),
  ];
  const failed = run("r1", RunState.FAILED, { commitId: "c4" });
  const events = buildEvents(
    [
      {
        repo: "alice/demo",
        commits,
        tags: new Map([["c2", ["v1"]]]),
        runs: [failed, run("r0", RunState.SUCCEEDED, { commitId: "c3" })],
      },
    ],
    { since: Number(at(10).seconds) },
  );
  const groups = events.filter((e) => e.kind === "commits");
  assert.deepEqual(
    groups.map((g) => [g.author, g.commits.map((c) => c.id)]).sort(),
    [
      ["alice", ["c3", "c2"]],
      ["bob", ["c4"]],
    ],
  );
  assert.deepEqual(countEvents(events), {
    commits: 3,
    release: 1,
    ci: 1,
    mirror: 0,
    runner: 0,
  });

  const forge = (runs) => ({
    repos: [
      {
        repo: { defaultBranch: "refs/heads/main" },
        key: "alice/demo",
        commits,
        tags: new Map(),
        runs,
      },
    ],
    runs,
    runners: [],
    mirrors: [],
  });
  // a manual run of the tip counts for the default branch; a run of another commit does not
  assert.equal(attention(forge([failed])).branches.length, 1);
  assert.equal(
    attention(forge([run("r2", RunState.FAILED, { commitId: "c1" })])).branches
      .length,
    0,
  );
  // the last finished run decides; a running one does not mask it
  const newer = run("r3", RunState.RUNNING, { ref: "refs/heads/main" });
  assert.equal(attention(forge([newer, failed])).branches.length, 1);
  const fixed = run("r4", RunState.SUCCEEDED, { ref: "refs/heads/main" });
  assert.equal(attention(forge([fixed, failed])).branches.length, 0);
  // other branches never count
  assert.equal(
    attention(forge([run("r5", RunState.FAILED, { ref: "refs/heads/dev" })]))
      .branches.length,
    0,
  );
});

test("uniqueCommits and buildEvents drop a commit listed twice", async (t) => {
  const vite = await createServer({
    configFile: false,
    server: { middlewareMode: true, ws: false },
  });
  t.after(() => vite.close());
  const { uniqueCommits } = await vite.ssrLoadModule("/src/lib/data/forge.ts");
  const { buildEvents } = await vite.ssrLoadModule("/src/lib/data/events.ts");
  const a = commit("a", "alice", 20, 9);
  const b = commit("b", "alice", 20, 8);
  assert.deepEqual(
    uniqueCommits([a, b, a, b]).map((c) => c.id),
    ["a", "b"],
  );
  // overlapping pages: the same commit twice, tagged once
  const ev = buildEvents([
    {
      repo: "o/r",
      commits: [a, b, a],
      tags: new Map([["a", ["v1"]]]),
      runs: [],
    },
  ]);
  const commits = ev
    .filter((e) => e.kind === "commits")
    .flatMap((e) => e.commits);
  assert.deepEqual(
    commits.map((c) => c.id),
    ["a", "b"],
  );
  assert.equal(ev.filter((e) => e.kind === "release").length, 1);
});

test("keyed each blocks use keys unique for real data", async () => {
  const { readFileSync } = await import("node:fs");
  const src = (f) =>
    readFileSync(new URL(`../src/${f}`, import.meta.url), "utf8");
  // the same commit in two repositories, steps sharing a name, one path changed twice
  assert.match(
    src("lib/components/ActivityChart.svelte"),
    /as \{ c, repo \} \(`\$\{repo\}\/\$\{c\.id\}`\)/,
  );
  assert.match(
    src("lib/views/RepoView.svelte"),
    /as ch \(`\$\{ch\.kind\}:\$\{ch\.path\}`\)/,
  );
  for (const f of [
    "lib/views/RunView.svelte",
    "lib/components/StepColumns.svelte",
  ])
    assert.doesNotMatch(src(f), /\(st?\.name\)\}/);
});

test("lastDays gives distinct days across a 25 h daylight-saving day", async (t) => {
  const vite = await createServer({
    configFile: false,
    server: { middlewareMode: true, ws: false },
  });
  t.after(() => vite.close());
  const { lastDays } = await vite.ssrLoadModule("/src/lib/data/forge.ts");
  const days = lastDays(90, new Date(2026, 9, 31, 23, 21));
  assert.equal(new Set(days.map((d) => d.k)).size, 90);
  assert.equal(days.at(-1).k, "2026-10-31");
});
