import { expect, test } from "@playwright/test";
import { authenticator, enroll, enrollLink } from "./support";

for (const path of ["/", "/alice/demo?ref=feature&history=1"]) {
  test(`Pulse restores daily ribbons at ${path}`, async ({ page }) => {
    await authenticator(page);
    await enroll(page, enrollLink());
    await page.getByRole("link", { name: "Continue to kajiba" }).click();
    await page.goto(path);
    const pulse = page.getByRole("region", { name: "Pulse", exact: true });
    const figure = pulse.getByRole("figure");
    const header = page
      .locator("a[aria-expanded]")
      .filter({ hasText: "Pulse" })
      .first();
    await expect(header).toHaveText(/^[▾\s]*Pulse\s*$/);
    expect(
      await header.evaluate((el) => getComputedStyle(el).borderBottomWidth),
    ).toBe("0px");
    expect(
      await pulse.evaluate((el) => getComputedStyle(el).borderBottomWidth),
    ).toBe("0px");
    await expect(
      pulse.getByRole("link", { name: "7 days", exact: true }),
    ).toHaveCount(0);
    await expect(
      pulse.getByRole("link", { name: "1 year", exact: true }),
    ).toHaveCount(0);
    if (path === "/") {
      const period = pulse.getByRole("navigation", { name: "Period" });
      await expect(period.getByRole("link")).toHaveText(["30 days", "90 days"]);
      await expect(
        period.getByRole("link", { name: "90 days" }),
      ).toHaveAttribute("aria-current", "true");
      await expect(figure.locator("div[title]")).toHaveCount(90);
      await expect(
        pulse.getByRole("img", { name: "CI outcome per day" }),
      ).toBeVisible();
      await period.getByRole("link", { name: "30 days" }).click();
      await expect(page).toHaveURL(/days=30/);
      await expect(figure.locator("div[title]")).toHaveCount(30);
      await period.getByRole("link", { name: "90 days" }).click();
      await expect(figure.locator("div[title]")).toHaveCount(90);
    } else {
      await expect(figure.getByRole("button")).toHaveCount(90);
      const list = pulse.getByRole("list", { name: "Pulse commits" });
      await expect(list.getByRole("link")).toHaveCount(2);
      const day = figure
        .getByRole("button", { name: /, [1-9]\d* commits/ })
        .last();
      await day.focus();
      await page.keyboard.press("Enter");
      await expect(page).toHaveURL(/day=\d{4}-\d{2}-\d{2}/);
      await expect(page).toHaveURL(/ref=feature/);
      await expect(page).toHaveURL(/history=1/);
      const selected = new URL(page.url()).searchParams.get("day");
      const contents = await list.innerText();
      await list.getByRole("link").first().click();
      await expect(page).not.toHaveURL(/[?&]at=/);
      expect(new URL(page.url()).searchParams.get("day")).toBe(selected);
      await expect(list).toHaveText(contents, { useInnerText: true });
      expect(await list.evaluate((el) => getComputedStyle(el).maxHeight)).toBe(
        "192px",
      );
    }
  });
}
