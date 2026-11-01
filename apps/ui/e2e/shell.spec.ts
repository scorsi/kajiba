import { expect, test } from "@playwright/test";
import { authenticator, enroll, enrollLink } from "./support";

test("the shell toggles from the full header zone and marks the current entry", async ({
  page,
}) => {
  await authenticator(page);
  await enroll(page, enrollLink());
  await page.getByRole("link", { name: "Continue to kajiba" }).click();
  await page.goto("/alice/demo?path=docs&history=1");
  const toggle = page.getByRole("button", { name: /kajiba.*overview/ });
  const header = page.locator("header");
  const box = await toggle.boundingBox();
  const headerBox = await header.boundingBox();
  expect(box!.width).toBeGreaterThan(headerBox!.width / 2);
  await toggle.click({ position: { x: box!.width - 8, y: box!.height / 2 } });
  await expect(toggle).toHaveAttribute("aria-expanded", "true");
  await expect(page).toHaveURL(/path=docs&history=1&drawer=1$/);
  const overview = page.getByRole("region", { name: "kajiba overview" });
  const repo = overview.getByRole("link", { name: /alice\/demo/ });
  await expect(repo).toHaveAttribute("aria-current", "page");
  await expect(repo).toHaveCSS("cursor", "pointer");
  await expect(
    overview.getByRole("region", { name: "Runners", exact: true }),
  ).toHaveCount(0);
  await toggle.focus();
  await page.keyboard.press("Space");
  await expect(toggle).toHaveAttribute("aria-expanded", "false");
  await expect(page).toHaveURL(/path=docs&history=1$/);
  await page.keyboard.press("Enter");
  await expect(toggle).toHaveAttribute("aria-expanded", "true");
  await repo.click();
  await expect(page).toHaveURL(/\/alice\/demo$/);
  await expect(toggle).toHaveAttribute("aria-expanded", "false");

  const run = process.env.KAJIBA_E2E_PENDING_RUN;
  if (!run)
    throw new Error(
      "KAJIBA_E2E_PENDING_RUN is unset: run through nix run .#e2e",
    );
  for (const [path, label] of [
    ["/", "Activity"],
    ["/-/account", "Account"],
    ["/-/admin", "Administration"],
    [`/-/runs/${run}`, "Activity"],
  ]) {
    await page.goto(path + "?drawer=1");
    const pages = overview.getByRole("navigation", { name: "Pages" });
    const current = pages.getByRole("link", { name: label, exact: true });
    await expect(current).toHaveAttribute("aria-current", "page");
    await expect(pages.locator('[aria-current="page"]')).toHaveCount(1);
    const other = pages.getByRole("link", {
      name: label === "Account" ? "Activity" : "Account",
      exact: true,
    });
    expect(
      await current.evaluate((el) => getComputedStyle(el).backgroundColor),
    ).not.toBe(
      await other.evaluate((el) => getComputedStyle(el).backgroundColor),
    );
  }
});
