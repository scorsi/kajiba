import { expect, test } from "@playwright/test";

test("the server serves the sign-in page", async ({ page }) => {
  await page.goto("/-/login");
  await expect(
    page.getByRole("heading", { name: "Welcome back" }),
  ).toBeVisible();
});
