import assert from "node:assert/strict";
import { test } from "node:test";
import { createServer } from "vite";

test("log windows bound DOM rows, retain filtered anchors and measure wrapped lines", async (t) => {
  const vite = await createServer({
    configFile: false,
    server: { middlewareMode: true, ws: false },
  });
  t.after(() => vite.close());
  const { linePositions, lineWindow, lineTop } = await vite.ssrLoadModule(
    "/src/lib/log-window.ts",
  );
  const indices = Uint32Array.from({ length: 50_000 }, (_, i) => i);
  const heights = new Map([
    [10, 100],
    [40_000, 60],
  ]);
  const p = linePositions(indices, heights);
  for (const top of [0, 400, 800_000, 999_999, 2_000_000]) {
    const w = lineWindow(p, top, 600);
    assert.ok(w.end - w.start <= 47);
    assert.equal(w.before + p[w.end] - p[w.start] + w.after, p.at(-1));
  }
  assert.equal(p[11] - p[10], 100);
  const deep = lineWindow(p, lineTop(p, 40_000, 600), 600);
  assert.ok(deep.start <= 40_000 && deep.end > 40_000);
  const filtered = linePositions(new Uint32Array([10, 40_000]), heights);
  assert.deepEqual([...filtered], [0, 100, 160]);
  assert.deepEqual(lineWindow(new Float64Array([0]), 0, 600), {
    start: 0,
    end: 0,
    before: 0,
    after: 0,
  });
});

test("logs beyond browser scroll limits still reach deep anchors and the tail", async (t) => {
  const vite = await createServer({
    configFile: false,
    server: { middlewareMode: true, ws: false },
  });
  t.after(() => vite.close());
  const { linePositions, lineWindow, lineTop } = await vite.ssrLoadModule(
    "/src/lib/log-window.ts",
  );
  const p = linePositions(
    Uint32Array.from({ length: 1_000_000 }, (_, i) => i),
    new Map(),
  );
  for (const index of [0, 500_000, 999_999]) {
    const w = lineWindow(p, lineTop(p, index, 600), 600);
    assert.ok(w.start <= index && w.end > index);
    assert.ok(w.end - w.start <= 47);
    assert.equal(w.before + p[w.end] - p[w.start] + w.after, 8_000_000);
  }
});

test("log segments share line records and retain original anchor offsets", async (t) => {
  const vite = await createServer({
    configFile: false,
    server: { middlewareMode: true, ws: false },
  });
  t.after(() => vite.close());
  const { segments } = await vite.ssrLoadModule("/src/lib/data/run.ts");
  const lines = [
    { text: "start", stream: "out", offsetSeconds: 1 },
    { text: "check", stream: "out", offsetSeconds: 2, group: "test" },
    { text: "error: check", stream: "err", offsetSeconds: 3, group: "test" },
  ];
  const s = segments(lines, 10);
  assert.equal(s.length, 2);
  assert.equal(s[1].offset, 11);
  assert.equal(s[1].lines[0], lines[1]);
  assert.equal(s[1].failed, true);
  assert.equal(s[1].start, 2);
  assert.equal(s[1].end, 3);
});
