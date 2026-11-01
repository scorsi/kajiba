import assert from "node:assert/strict";
import { test } from "node:test";
import { createServer } from "vite";

test("diff lines reuse file syntax highlighting and escape repository content", async (t) => {
  const vite = await createServer({
    configFile: false,
    server: { middlewareMode: true, ws: false },
  });
  t.after(() => vite.close());
  const { highlightDiff, highlight } = await vite.ssrLoadModule(
    "/src/lib/highlight.ts",
  );
  const { lineDiff } = await vite.ssrLoadModule("/src/lib/diff.ts");
  const hunks = lineDiff(
    'const old = "<script>";\n',
    'const next = "<img src=x onerror=alert(1)>";\n',
  );
  const html = await highlightDiff(hunks, "javascript");
  for (let h = 0; h < hunks.length; h++)
    for (let l = 0; l < hunks[h].lines.length; l++) {
      assert.equal(
        html[h][l],
        await highlight(hunks[h].lines[l].text, "javascript"),
      );
      assert.match(html[h][l], /hljs-keyword/);
      assert.doesNotMatch(html[h][l], /<script>|<img/);
    }
  assert.equal(await highlightDiff(hunks), undefined);
  assert.deepEqual(await highlightDiff(hunks, "unknown"), [
    [undefined, undefined],
  ]);
  assert.equal(
    await highlightDiff(
      [{ lines: [{ text: "x".repeat(256 * 1024 + 1) }] }],
      "javascript",
    ),
    undefined,
  );
});
