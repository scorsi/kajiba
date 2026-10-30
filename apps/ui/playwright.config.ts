// e2e suite (0015): run by `nix run .#e2e`, which starts the daemons and sets
// KAJIBA_E2E_URL; browsers come from nixpkgs (PLAYWRIGHT_BROWSERS_PATH), never downloaded.
import { defineConfig, devices } from "@playwright/test";

export default defineConfig({
  testDir: "e2e",
  // The scenarios share one server and one user: run them in order.
  workers: 1,
  fullyParallel: false,
  forbidOnly: true,
  reporter: "list",
  // The suite may run from the read-only Nix store: results go where the harness says.
  outputDir: process.env.KAJIBA_E2E_OUTPUT ?? "test-results",
  use: {
    baseURL: process.env.KAJIBA_E2E_URL,
    trace: "retain-on-failure",
  },
  projects: [{ name: "chromium", use: { ...devices["Desktop Chrome"] } }],
});
