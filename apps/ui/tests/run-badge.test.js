import assert from "node:assert/strict";
import { test } from "node:test";
import { createServer } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";

test("every run state maps to its shared badge icon, colour and label", async (t) => {
  const vite = await createServer({
    configFile: false,
    plugins: [svelte({ configFile: false })],
    server: { middlewareMode: true, ws: false },
  });
  t.after(() => vite.close());
  const { default: RunBadge } = await vite.ssrLoadModule(
    "/src/lib/components/RunBadge.svelte",
  );
  const { RunState } = await vite.ssrLoadModule("/src/lib/api.ts");
  const { RunState: WireRunState } = await vite.ssrLoadModule(
    "/src/lib/gen/kajiba/v1/run_pb.ts",
  );
  for (const name of [
    "UNSPECIFIED",
    "QUEUED",
    "RUNNING",
    "SUCCEEDED",
    "FAILED",
    "CANCELLED",
  ])
    assert.equal(RunState[name], WireRunState[name], name);
  const { hasActiveRuns } = await vite.ssrLoadModule("/src/lib/data/forge.ts");
  const { render } = await vite.ssrLoadModule("svelte/server");
  assert.equal(hasActiveRuns([]), false);
  for (const [state, label, colour, icon] of [
    [
      RunState.UNSPECIFIED,
      "unknown",
      "text-fg-subtle",
      'stroke-dasharray="2 2"',
    ],
    [RunState.QUEUED, "queued", "text-fg-subtle", 'stroke-dasharray="2 2"'],
    [RunState.RUNNING, "running", "text-accent", "motion-safe:animate-spin"],
    [RunState.SUCCEEDED, "passed", "text-ok", "M3.5 6.2"],
    [RunState.FAILED, "failed", "text-danger", "M4.2 4.2"],
    [RunState.CANCELLED, "cancelled", "text-fg-subtle", "M3.2 8.8"],
  ]) {
    assert.equal(
      hasActiveRuns([{ state }]),
      state === RunState.QUEUED || state === RunState.RUNNING,
    );
    for (const visibleLabel of [false, true]) {
      const html = render(RunBadge, {
        props: { state, label: visibleLabel },
      }).body;
      assert.ok(html.includes(label), html);
      assert.ok(html.includes(colour), html);
      assert.ok(html.includes(icon), html);
      assert.equal(html.includes("animate-spin"), state === RunState.RUNNING);
    }
  }
});
