import assert from "node:assert/strict";
import { test } from "node:test";
import { createServer } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";

test("dur switches to hours at 60 minutes and drops seconds", async (t) => {
  const vite = await createServer({
    configFile: false,
    server: { middlewareMode: true, ws: false },
  });
  t.after(() => vite.close());
  const { dur } = await vite.ssrLoadModule("/src/lib/data/forge.ts");
  for (const [seconds, expected] of [
    [0, "0s"],
    [59, "59s"],
    [60, "1m00s"],
    [1638, "27m18s"],
    [3599, "59m59s"],
    [3600, "1h0m"],
    [3659, "1h0m"],
    [6798, "1h53m"],
    [7199, "1h59m"],
    [7200, "2h0m"],
    [90061, "25h1m"],
  ]) {
    assert.equal(dur(seconds), expected, `${seconds} seconds`);
  }
});

test("duration bars share a fixed track and scale independently of label length", async (t) => {
  const vite = await createServer({
    configFile: false,
    plugins: [svelte({ configFile: false })],
    server: { middlewareMode: true, ws: false },
  });
  t.after(() => vite.close());
  const { default: DurationBar } = await vite.ssrLoadModule(
    "/src/lib/components/DurationBar.svelte",
  );
  const { render } = await vite.ssrLoadModule("svelte/server");
  for (const [seconds, max, width] of [
    [0, 0, 0],
    [1638, 7200, (1638 / 7200) * 100],
    [6798, 7200, (6798 / 7200) * 100],
    [7200, 7200, 100],
    [7200, 3600, 100],
  ]) {
    const html = render(DurationBar, { props: { seconds, max } }).body;
    assert.match(html, /grid-cols-\[6rem_8ch\]/);
    assert.match(html, /tabular text-right/);
    assert.equal(Number(html.match(/width: ([\d.]+)%;/)[1]), width);
  }
});
