import { expect, test } from "@playwright/test";

test("the server serves the sign-in page", async ({ page }) => {
  await page.goto("/-/login");
  await expect(
    page.getByRole("heading", { name: "Welcome back" }),
  ).toBeVisible();
});

test("sign-in card is centered and explains administrator enrollment", async ({
  page,
}) => {
  for (const width of [360, 1280]) {
    await page.setViewportSize({ width, height: 800 });
    await page.goto("/-/login?next=/alice/demo");
    const card = page.getByRole("region", { name: "Sign in", exact: true });
    await expect(card).toContainText(
      "Accounts are created through an administrator's enrollment link.",
    );
    await expect(page.getByText("No account yet")).toHaveCount(0);
    await expect(
      page.getByRole("button", { name: "Sign in with a passkey" }),
    ).toHaveCSS("cursor", "pointer");
    const main = await page.locator("main").boundingBox();
    const box = await card.boundingBox();
    expect(
      Math.abs(box!.x + box!.width / 2 - (main!.x + main!.width / 2)),
    ).toBeLessThan(2);
    expect(
      Math.abs(box!.y + box!.height / 2 - (main!.y + main!.height / 2)),
    ).toBeLessThan(2);
    expect(box!.width).toBeLessThan(width);
    await expect(card).toContainText("Then back to /alice/demo");
  }
});
