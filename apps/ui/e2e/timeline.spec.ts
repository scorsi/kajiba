// Activity composed in the browser (0145): the fixture's commits under their author and day, a
// failed run on the default branch under Needs attention, and the reader sees the same
// repository. Sorts after ci.spec.ts (the runner is started by then); it fails one run on main
// through the fake nix's flag file (nix/apps/e2e.nix).
import { execFileSync } from "node:child_process";
import { rmSync, writeFileSync } from "node:fs";
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

test("activity: commits by author, a failed default-branch run, the reader's view", async ({
  page,
  browser,
}) => {
  const flag = env("KAJIBA_E2E_FAIL_FLAG");
  writeFileSync(flag, "");
  let id: string;
  try {
    id = execFileSync(
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
    await signedIn(page, "alice");
    await expect(async () => {
      await page.goto(`/-/runs/${id}`);
      await expect(page.locator("[data-failure]")).toBeVisible({
        timeout: 2_000,
      });
    }).toPass({ timeout: 60_000 });
  } finally {
    rmSync(flag, { force: true });
  }

  await page.goto("/?history=1");
  const attention = page.getByRole("status").filter({
    hasText: "Needs attention",
  });
  await expect(attention).toContainText("alice/demo");
  await expect(attention).toContainText("failing");
  await expect(attention.locator(`a[href="/-/runs/${id}"]`)).toBeVisible();
  await expect(
    page.getByText("Add the README and a guide").first(),
  ).toBeVisible();
  await expect(page.getByText("Alice").first()).toBeVisible();

  // The reader sees the same repository, with no administrators' lines.
  const other = await browser.newContext();
  const readerPage = await other.newPage();
  await signedIn(readerPage, "reader");
  await readerPage.goto("/");
  await expect(
    readerPage.locator('a[href="/alice/demo"]').first(),
  ).toBeVisible();
  await expect(
    readerPage.getByRole("status").filter({ hasText: "offline" }),
  ).toHaveCount(0);
  await other.close();
});
