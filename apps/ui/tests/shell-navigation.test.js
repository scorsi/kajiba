import assert from "node:assert/strict";
import { test } from "node:test";
import { createServer } from "vite";

test("shell entries stay current on nested routes without matching adjacent names", async (t) => {
  const vite = await createServer({
    configFile: false,
    server: { middlewareMode: true, ws: false },
  });
  t.after(() => vite.close());
  const { isCurrentPage } = await vite.ssrLoadModule(
    "/src/lib/shell-navigation.ts",
  );
  for (const [path, href] of [
    ["/", "/"],
    ["/-/runs/abc", "/"],
    ["/-/runs/abc/steps", "/"],
    ["/-/account", "/-/account"],
    ["/-/account/keys", "/-/account"],
    ["/-/admin/runners", "/-/admin"],
    ["/alice/demo/tree/main", "/alice/demo"],
  ])
    assert.equal(isCurrentPage(path, href), true, path);
  for (const [path, href] of [
    ["/alice/demo", "/"],
    ["/-/account", "/"],
    ["/-/administrator", "/-/admin"],
    ["/-/runs-other/abc", "/"],
    ["/alice/demo-other", "/alice/demo"],
  ])
    assert.equal(isCurrentPage(path, href), false, path);
});
