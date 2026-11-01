// Administration on the real API (0139, 0144): the administrator sees the harness runner
// connected, then its current run, and the server facts; another user gets 403 and no entry.
// Its name sorts after ci.spec.ts, which starts the runner.
import { execFileSync } from "node:child_process";
import { expect, test, type Page } from "@playwright/test";
import { authenticator, enroll, enrollLink } from "./support";

const env = (name: string) => {
  const value = process.env[name];
  if (!value) throw new Error(`${name} is unset: run through nix run .#e2e`);
  return value;
};

async function signedIn(page: Page, user: string) {
  await authenticator(page);
  await enroll(page, enrollLink(user), user);
  await page.getByRole("link", { name: "Continue to kajiba" }).click();
}

test("the administrator sees the runner, its current run and the server", async ({
  page,
}) => {
  await signedIn(page, "alice");
  await page.goto("/-/admin");
  const runner = page.locator("#runner-e2e");
  await expect(runner).toContainText("e2e");
  await expect(runner).toContainText("idle", { timeout: 30_000 });
  await expect(page.getByRole("region", { name: "Runners" })).toContainText(
    "e2e-host",
  );

  const id = execFileSync(
    "kajiba",
    [
      "--token-file",
      env("KAJIBA_E2E_TOKEN_FILE"),
      "--development",
      "ci",
      "run",
      "--detach",
      "alice/demo",
      env("KAJIBA_E2E_MAIN_SHA"),
    ],
    { encoding: "utf8", stdio: ["ignore", "pipe", "ignore"] },
  ).trim();
  await expect(async () => {
    await page.reload();
    await expect(runner.locator(`a[href="/-/runs/${id}"]`)).toBeVisible({
      timeout: 1_500,
    });
  }).toPass({ timeout: 30_000 });
  await expect(runner).toContainText("busy");
  await expect(async () => {
    await page.reload();
    await expect(runner).toContainText("idle", { timeout: 1_500 });
  }).toPass({ timeout: 30_000 });

  // The declared repository has no mirror; the server facts are the server's.
  await page.goto("/-/admin?mirrors=1");
  await expect(page.getByText("No mirror declared.")).toBeVisible();
  const server = page.getByRole("complementary", { name: "Server" });
  await expect(server).toContainText("Version");
  await expect(server).toContainText("generation 1");

  // The shell's runner summary.
  await page.goto("/?drawer=1");
  await expect(
    page.getByRole("link", { name: "Administration" }),
  ).toBeVisible();
  await expect(page.locator('a[href="/-/admin#runner-e2e"]')).toBeVisible();
});

test("another user gets 403 and no administration entry", async ({ page }) => {
  await signedIn(page, "reader");
  await page.goto("/-/admin");
  await expect(page.getByText("403 · no access")).toBeVisible();
  await page.goto("/?drawer=1");
  await expect(
    page
      .getByRole("navigation", { name: "Pages" })
      .getByRole("link", { name: "Account", exact: true }),
  ).toBeVisible();
  await expect(page.getByRole("link", { name: "Administration" })).toHaveCount(
    0,
  );
  await expect(page.locator('a[href="/-/admin#runners"]')).toHaveCount(0);
});
