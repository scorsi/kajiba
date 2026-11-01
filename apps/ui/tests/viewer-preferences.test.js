import assert from "node:assert/strict";
import { test } from "node:test";
import { createServer } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";

test("viewer preferences default on, persist separately and tolerate unavailable storage", async (t) => {
  const vite = await createServer({
    configFile: false,
    plugins: [svelte({ configFile: false })],
    server: { middlewareMode: true, ws: false },
  });
  t.after(() => {
    delete globalThis.localStorage;
    return vite.close();
  });
  const saved = new Map();
  globalThis.localStorage = {
    getItem: (key) => saved.get(key) ?? null,
    setItem: (key, value) => saved.set(key, value),
  };
  const { preferences, loadPreferences, setHideFixedFailures } =
    await vite.ssrLoadModule("/src/lib/viewer-preferences.svelte.ts");
  loadPreferences("alice");
  assert.equal(preferences.hideFixedFailures, true);
  setHideFixedFailures(false);
  loadPreferences("reader");
  assert.equal(preferences.hideFixedFailures, true);
  loadPreferences("alice");
  assert.equal(preferences.hideFixedFailures, false);
  globalThis.localStorage = {
    getItem() {
      throw new Error("unavailable");
    },
    setItem() {
      throw new Error("unavailable");
    },
  };
  loadPreferences("alice");
  assert.equal(preferences.hideFixedFailures, true);
  setHideFixedFailures(false);
  assert.equal(preferences.hideFixedFailures, false);
});
