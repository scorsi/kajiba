import assert from "node:assert/strict";
import { test } from "node:test";
import { createServer } from "vite";

test("old tree and commits paths map to the query form", async (t) => {
  const vite = await createServer({
    configFile: false,
    server: { middlewareMode: true, ws: false },
  });
  t.after(() => vite.close());
  const { splitRefPath, repoQuery } = await vite.ssrLoadModule(
    "/src/lib/legacy-urls.ts",
  );
  const refs = ["refs/heads/main", "refs/heads/feature/x", "refs/tags/v1"];
  assert.deepEqual(splitRefPath("feature/x/src/a.rs", refs), {
    ref: "feature/x",
    path: "src/a.rs",
  });
  assert.deepEqual(splitRefPath("main", refs), { ref: "main", path: "" });
  assert.deepEqual(splitRefPath("abc123/docs", refs), {
    ref: "abc123",
    path: "docs",
  });
  assert.equal(repoQuery("a/b", "main", "x y"), "/a/b?ref=main&path=x+y");
  assert.equal(repoQuery("a/b", "", ""), "/a/b");
});
