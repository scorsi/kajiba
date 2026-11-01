import assert from "node:assert/strict";
import { test } from "node:test";
import { createServer } from "vite";

test("run actions require write access and enabled, unarchived CI", async (t) => {
  const vite = await createServer({
    configFile: false,
    server: { middlewareMode: true, ws: false },
  });
  t.after(() => vite.close());
  const { canManageRuns, Role, CiMode } =
    await vite.ssrLoadModule("/src/lib/api.ts");
  const repo = {
    owner: "alice",
    archived: false,
    ciMode: CiMode.MANUAL,
    permissions: [
      { user: "writer", role: Role.WRITE },
      { user: "reader", role: Role.READ },
    ],
  };
  for (const [user, admin, allowed] of [
    ["alice", false, true],
    ["writer", false, true],
    ["reader", false, false],
    ["unknown", false, false],
    ["admin", true, true],
  ]) {
    const viewer = { user, admin };
    assert.equal(canManageRuns(repo, viewer), allowed);
    assert.equal(canManageRuns({ ...repo, archived: true }, viewer), false);
    assert.equal(canManageRuns({ ...repo, ciMode: CiMode.OFF }, viewer), false);
  }
  assert.equal(canManageRuns(undefined, { user: "alice" }), false);
  assert.equal(canManageRuns(repo, undefined), false);
});
