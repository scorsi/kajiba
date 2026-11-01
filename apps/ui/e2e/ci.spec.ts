// CI runs on the real API (0010, 0086, 0109, 0112): a run queued before any runner connected says
// why; once the harness's runner starts it succeeds; a failing run shows its summary; both
// appear in the repository's Pipelines band. The runner is real, its `nix` a script (nix/apps/e2e.nix).
import { execFileSync } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
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
    await expect(
      page.getByRole("region", { name: "What changed" }),
    ).toHaveCount(0);
    const consolePane = log.locator("..");
    const bounds = await consolePane.boundingBox();
    expect(bounds).not.toBeNull();
    expect(
      Math.abs(bounds!.y + bounds!.height - page.viewportSize()!.height),
    ).toBeLessThan(4);
    await expect(consolePane).toHaveCSS("overflow-y", "auto");
    const gutterFits = await log
      .locator('a[aria-label^="Line "]')
      .first()
      .evaluate((a) => {
        const context = document.createElement("canvas").getContext("2d")!;
        context.font = getComputedStyle(a).font;
        return (
          a.getBoundingClientRect().width >=
          context.measureText("999999").width + 12
        );
      });
    expect(gutterFits).toBe(true);
    await page.getByRole("link", { name: /What changed/ }).click();
    await expect(
      page.getByRole("region", { name: "What changed" }),
    ).toBeVisible();
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
    await expect(
      page.getByRole("region", { name: "What changed" }),
    ).toHaveCount(0);

    await page.goto("/alice/demo");
    const pipelines = page.getByRole("region", { name: "Pipelines" });
    await expect(pipelines.locator(`a[href="/-/runs/${id}"]`)).toBeVisible();
  });

  test("a link to another run of the ref opens it on the first click", async ({
    page,
  }) => {
    await signedIn(page);
    const first = env("KAJIBA_E2E_PENDING_RUN");
    await page.goto(`/-/runs/${first}`);
    const heading = (id: string) =>
      page.getByRole("heading", { name: `Run #${id.slice(-8).toLowerCase()}` });
    await expect(heading(first)).toBeVisible();

    // Same route, other id: each click shows the clicked run at once, with no reload.
    const others = pulse(page).locator(
      'a[href^="/-/runs/"]:not([aria-current])',
    );
    await expect(others.first()).toBeVisible();
    // A slow backend: the clicked run must not leave the previous one on screen.
    await page.route("**/RunService/GetRun", async (route) => {
      await new Promise((done) => setTimeout(done, 700));
      await route.continue();
    });
    const next = (await others.first().getAttribute("href"))!.split("/").pop()!;
    await others.first().click();
    await expect(page).toHaveURL(new RegExp(`/-/runs/${next}$`));
    await expect(heading(first)).toHaveCount(0, { timeout: 300 });
    await expect(heading(next)).toBeVisible();

    await pulse(page).locator(`a[href="/-/runs/${first}"]`).first().click();
    await expect(heading(first)).toBeVisible();
    await expect(heading(next)).toHaveCount(0);
  });

  test("worktree runs carry their own identity and stay out of branch pipelines", async ({
    page,
  }) => {
    await signedIn(page);
    const dir = mkdtempSync(join(tmpdir(), "kajiba-worktree-"));
    try {
      const work = join(dir, "work");
      execFileSync("git", [
        "clone",
        "--quiet",
        `${env("KAJIBA_E2E_URL")}/alice/demo.git`,
        work,
      ]);
      // The harness fixture has no flake; stub the CLI's routing hint, as the runner's Nix already is.
      writeFileSync(join(dir, "nix"), "#!/bin/sh\nprintf '[]\\n'\n", {
        mode: 0o755,
      });
      const id = execFileSync(
        "kajiba",
        [
          "--token-file",
          env("KAJIBA_E2E_TOKEN_FILE"),
          "--development",
          "ci",
          "run",
          "--worktree",
          "--check",
          "unit",
          "--detach",
        ],
        {
          cwd: work,
          encoding: "utf8",
          env: { ...process.env, PATH: `${dir}:${process.env.PATH}` },
        },
      ).trim();
      const label = `worktree by alice on ${env("KAJIBA_E2E_MAIN_SHA").slice(0, 7)} · checks: unit`;
      await page.goto(`/-/runs/${id}`);
      await expect(pulse(page)).toContainText(label);
      await expect(
        page.getByRole("region", { name: "What changed" }),
      ).toHaveCount(0);
      await page.goto("/alice/demo");
      const pipelines = page.getByRole("region", { name: "Pipelines" });
      await expect(
        pipelines.getByRole("heading", { name: "Worktree runs" }),
      ).toBeVisible();
      await expect(pipelines.locator(`a[href="/-/runs/${id}"]`)).toHaveCount(1);
      await expect(pipelines.locator(`a[href="/-/runs/${id}"]`)).toContainText(
        label,
      );
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });
});
