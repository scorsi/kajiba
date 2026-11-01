import assert from "node:assert/strict";
import { test } from "node:test";
import { createServer } from "vite";

test("worktree labels show user, base and checks; they never fail a branch", async (t) => {
  const vite = await createServer({
    configFile: false,
    server: { middlewareMode: true, ws: false },
  });
  t.after(() => vite.close());
  const { runLabel } = await vite.ssrLoadModule("/src/lib/run-label.ts");
  const { attention } = await vite.ssrLoadModule("/src/lib/data/events.ts");
  const { RunState } = await vite.ssrLoadModule("/src/lib/api.ts");
  const run = {
    trigger: "worktree",
    triggeredBy: "alice",
    baseCommitId: "abcdef0123456789",
    checks: ["vm", "unit"],
    ref: "",
    commitId: "tip",
    state: RunState.FAILED,
  };
  assert.equal(
    runLabel(run),
    "worktree by alice on abcdef0 · checks: vm, unit",
  );
  assert.equal(
    runLabel({ ...run, checks: [] }),
    "worktree by alice on abcdef0",
  );
  assert.equal(
    runLabel({ ...run, baseCommitId: "", checks: [] }),
    "worktree by alice on unknown base",
  );
  assert.equal(
    runLabel({ ...run, trigger: "push", ref: "refs/heads/main" }),
    "main",
  );
  const forge = (runs) => ({
    repos: [
      {
        key: "alice/demo",
        repo: { defaultBranch: "refs/heads/main" },
        commits: [{ id: "tip" }],
        runs,
      },
    ],
    runs,
    runners: [],
    mirrors: [],
  });
  assert.equal(attention(forge([run])).branches.length, 0);
  const failed = { ...run, trigger: "push", ref: "refs/heads/main" };
  assert.equal(
    attention(forge([{ ...run, state: RunState.SUCCEEDED }, failed])).branches
      .length,
    1,
  );
});
