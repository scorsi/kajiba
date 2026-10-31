// CI runs on the real API (0010, 0086, 0109, 0112): a run queued before any runner connected says
// why; once the harness's runner starts it succeeds; a failing run shows its summary; both
// appear in the repository's Pipelines band. The runner is real, its `nix` a script (nix/apps/e2e.nix).
import { execFileSync } from "node:child_process";
import { writeFileSync } from "node:fs";
import { expect, test, type Page } from "@playwright/test";
import { authenticator, enroll, enrollLink } from "./support";

const env = (name: string) => {
  const value = process.env[name];
  if (!value) throw new Error(`${name} is unset: run through nix run .#e2e`);
  return value;
};

/** `kajiba ci run --detach` with the harness's `ci:trigger` token; prints the run id. */
const trigger = (sha: string) =>
  execFileSync(
    "kajiba",
    [
      "--token-file",
      env("KAJIBA_E2E_TOKEN_FILE"),
      "--development",
      "ci",
      "run",
      "--detach",
      "alice/demo",
      sha,
    ],
    { encoding: "utf8", stdio: ["ignore", "pipe", "ignore"] },
  ).trim();

async function signedIn(page: Page) {
  await authenticator(page);
  await enroll(page, enrollLink());
  await page.getByRole("link", { name: "Continue to kajiba" }).click();
}

const pulse = (page: Page) => page.getByRole("region", { name: "Pulse" });

test.describe.serial("CI runs", () => {
  test("a queued run says why, then the runner takes it and it passes", async ({
    page,
  }) => {
    await signedIn(page);
    const id = env("KAJIBA_E2E_PENDING_RUN");
    await page.goto(`/-/runs/${id}`);
    await expect(
      page.getByRole("heading", { name: `Run #${id.slice(-8).toLowerCase()}` }),
    ).toBeVisible();
    await expect(
      page.getByText("Waiting: no runner is connected."),
    ).toBeVisible();
    await expect(pulse(page)).toContainText("started manually by alice");

    // The page stays open: the log arrives live, then the terminal state, with no reload.
    // Count the WatchRun calls the page opens and aborts (the browser reports no failure for them).
    await page.addInitScript(() => {
      const w = window as unknown as {
        streams: { opened: number; closed: number };
      };
      w.streams = { opened: 0, closed: 0 };
      const fetch = window.fetch;
      window.fetch = (input, init) => {
        if (
          String(input instanceof Request ? input.url : input).endsWith(
            "RunService/WatchRun",
          )
        ) {
          w.streams.opened++;
          init?.signal?.addEventListener("abort", () => w.streams.closed++);
        }
        return fetch(input, init);
      };
    });
    const streams = () =>
      page.evaluate(
        () =>
          (window as unknown as { streams: { opened: number; closed: number } })
            .streams,
      );
    await page.reload();
    writeFileSync(env("KAJIBA_E2E_START_RUNNER"), "");
    const log = page.getByRole("log");
    await expect(log).toContainText("building 'fmt'", { timeout: 30_000 });
    await expect(pulse(page)).not.toContainText("passed");

    // Leaving the page closes its stream; a new page still opens one and the run still ends.
    await expect.poll(async () => (await streams()).opened).toBeGreaterThan(0);
    await page.getByRole("link", { name: "alice/demo" }).first().click();
    await expect.poll(async () => (await streams()).closed).toBeGreaterThan(0);
    await page.goto(`/-/runs/${id}`);
    await expect(log).toContainText("building 'fmt'");
    await expect(pulse(page)).toContainText("passed", { timeout: 30_000 });
    await expect(pulse(page)).toContainText("e2e-host");
  });

  test("a failing run shows its failure, and Pipelines lists the runs", async ({
    page,
  }) => {
    await signedIn(page);
    const id = trigger(env("KAJIBA_E2E_FEATURE_SHA"));
    await page.goto(`/-/runs/${id}`);
    await expect(async () => {
      await page.reload();
      await expect(page.locator("[data-failure]")).toContainText(
        "error: build of",
        { timeout: 2_000 },
      );
    }).toPass({ timeout: 60_000 });
    await expect(page.locator("[data-failure]")).toContainText("exit status 1");

    await page.goto("/alice/demo");
    const pipelines = page.getByRole("region", { name: "Pipelines" });
    await expect(pipelines.locator(`a[href="/-/runs/${id}"]`)).toBeVisible();
  });
});
