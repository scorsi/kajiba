import assert from "node:assert/strict";
import { test } from "node:test";
import { createServer } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";

test("runner occupancy distinguishes idle, partial, full and offline", async (t) => {
  const vite = await createServer({
    configFile: false,
    plugins: [svelte({ configFile: false })],
    server: { middlewareMode: true, ws: false },
  });
  t.after(() => vite.close());
  const { default: RunnerState } = await vite.ssrLoadModule(
    "/src/lib/components/RunnerState.svelte",
  );
  const { render } = await vite.ssrLoadModule("svelte/server");
  for (const [state, used, expected] of [
    [1, 0, "0/2 idle"],
    [2, 1, "1/2 busy · partially busy"],
    [2, 2, "2/2 busy · full"],
    [3, 0, "0/2 offline"],
    [3, 2, "2/2 offline"],
  ]) {
    for (const label of [true, false]) {
      const html = render(RunnerState, {
        props: { state, used, maxJobs: 2, label },
      }).body;
      assert.ok(html.includes(expected), html);
    }
  }
});
