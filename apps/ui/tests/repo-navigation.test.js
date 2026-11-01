import assert from "node:assert/strict";
import { test } from "node:test";
import { createServer } from "vite";

test("repository navigation preserves ref, history scope, day and band state", async (t) => {
  const vite = await createServer({
    configFile: false,
    server: { middlewareMode: true, ws: false },
  });
  t.after(() => vite.close());
  const { repoQuery, isMarkdown, sortChanges, localDateTime } =
    await vite.ssrLoadModule("/src/lib/repo-navigation.ts");
  const params = (s) => new URLSearchParams(s.replace(/^\?/, ""));
  const initial = params("ref=feature&history=1&day=2026-11-01&path=docs");
  const selected = params(repoQuery(initial, { at: "abc" }));
  assert.equal(selected.get("scope"), "docs");
  const file = params(repoQuery(selected, { path: "docs/intro.md" }));
  assert.equal(file.get("at"), "abc");
  assert.equal(file.get("scope"), "docs");
  assert.equal(params(repoQuery(file, { at: "def" })).get("scope"), "docs");
  const now = params(repoQuery(file, { at: null }));
  assert.equal(now.get("path"), "docs");
  assert.equal(now.get("at"), null);
  assert.equal(now.get("scope"), null);
  for (const q of [selected, file, now]) {
    assert.equal(q.get("ref"), "feature");
    assert.equal(q.get("history"), "1");
    assert.equal(q.get("day"), "2026-11-01");
  }
  const global = params(
    repoQuery(file, { path: null, scope: "", history: "1" }),
  );
  assert.equal(global.get("path"), null);
  assert.equal(global.get("scope"), null);
  assert.equal(global.get("ref"), "feature");
  const root = params(repoQuery(params("ref=feature"), { at: "abc" }));
  assert.equal(
    params(
      repoQuery(params(repoQuery(root, { path: "README.md" })), { at: null }),
    ).get("path"),
    null,
  );
  for (const path of ["README.md", "nested/Guide.MARKDOWN", "other.Md"])
    assert.ok(isMarkdown(path));
  assert.equal(isMarkdown("file.md.rs"), false);
  const changes = [
    { path: "z" },
    { path: "a/b" },
    { path: "README.md" },
    { path: "a" },
  ];
  assert.deepEqual(
    sortChanges(changes).map((c) => c.path),
    ["README.md", "a", "a/b", "z"],
  );
  assert.equal(changes[0].path, "z");
  assert.equal(localDateTime(undefined), "");
  const date = new Date("2026-11-01T13:14:15Z");
  assert.equal(
    localDateTime(date),
    date.toLocaleString(undefined, {
      year: "numeric",
      month: "2-digit",
      day: "2-digit",
      hour: "2-digit",
      minute: "2-digit",
      second: "2-digit",
      timeZoneName: "short",
    }),
  );
});

test("file mode follows changed-file and normal navigation, including the same path", async (t) => {
  const vite = await createServer({
    configFile: false,
    server: { middlewareMode: true, ws: false },
  });
  t.after(() => vite.close());
  const { repoQuery, fileViewMode, touchedPaths } = await vite.ssrLoadModule(
    "/src/lib/repo-navigation.ts",
  );
  const initial = new URLSearchParams("at=abc&path=src/main.rs&files=1");
  assert.equal(fileViewMode(initial), "file");
  const diff = new URLSearchParams(
    repoQuery(initial, { path: "src/main.rs", view: "diff" }).slice(1),
  );
  assert.equal(fileViewMode(diff), "diff");
  for (const next of [
    { path: "src/main.rs" },
    { path: "README.md" },
    { at: "def" },
    { at: null },
    { ref: "main" },
  ]) {
    assert.equal(
      fileViewMode(new URLSearchParams(repoQuery(diff, next).slice(1))),
      "file",
    );
  }
  assert.equal(
    fileViewMode(new URLSearchParams(repoQuery(diff, { files: "0" }).slice(1))),
    "diff",
  );
  assert.equal(fileViewMode(new URLSearchParams("view=unknown")), "file");
  const paths = touchedPaths([
    { path: "src/nested/main.rs" },
    { path: "src/lib.rs" },
    { path: "README.md" },
    { path: "deleted/file" },
  ]);
  assert.deepEqual([...paths].sort(), [
    "README.md",
    "deleted",
    "deleted/file",
    "src",
    "src/lib.rs",
    "src/nested",
    "src/nested/main.rs",
  ]);
  for (const path of [
    "",
    "sr",
    "src/other.rs",
    "src/nest",
    "src/nested/main.rs.bak",
  ])
    assert.equal(paths.has(path), false);
  assert.equal(touchedPaths([]).size, 0);
});
