import { expect, test } from "@playwright/test";
import { authenticator, enroll, enrollLink } from "./support";

for (const path of ["/", "/alice/demo?ref=feature&history=1"]) {
  test(`activity ranges change granularity and list selection at ${path}`, async ({
    page,
  }) => {
    await authenticator(page);
    await enroll(page, enrollLink());
    await page.getByRole("link", { name: "Continue to kajiba" }).click();
    await page.goto(path);
    const pulse = page.getByRole("region", { name: "Pulse", exact: true });
    const period = pulse.getByRole("navigation", { name: "Period" });
    const chart = pulse.getByRole("figure", { name: "Activity chart" });
    const list = pulse.getByRole("list", {
      name: path === "/" ? "Pulse activity" : "Pulse commits",
    });
    await expect(period.getByRole("link")).toHaveText([
      "7 days",
      "30 days",
      "1 year",
    ]);
    await expect(chart.locator('[data-granularity="day"] button')).toHaveCount(
      30,
    );
    await expect(list.getByRole("link").first()).toBeVisible();
    // Switching granularity reuses the loaded sample: it must not refetch history.
    let calls = 0;
    page.on("request", (req) => {
      if (
        /\/(ListCommits|ListRuns|ListRefs)$/.test(new URL(req.url()).pathname)
      )
        calls++;
    });
    await period.getByRole("link", { name: "7 days", exact: true }).click();
    await expect(
      period.getByRole("link", { name: "7 days", exact: true }),
    ).toHaveAttribute("aria-current", "true");
    const hours = chart.locator('[data-granularity="hour"] button');
    await expect(hours).toHaveCount(168);
    await expect(hours.first()).toHaveAttribute(
      "aria-label",
      /\d{4}-\d{2}-\d{2} 00:00, \d+ commits.*runs/s,
    );
    await hours.first().click();
    await expect(page).toHaveURL(/day=\d{4}-\d{2}-\d{2}T00/);
    await expect(list).toBeVisible();
    await period.getByRole("link", { name: "1 year", exact: true }).click();
    const weeks = chart.locator('[data-granularity="week"] button');
    await expect.poll(() => weeks.count()).toBeGreaterThanOrEqual(53);
    expect(await weeks.count()).toBeLessThanOrEqual(54);
    await expect(weeks.first()).toHaveAttribute(
      "aria-label",
      /Week of .*commits.*runs/s,
    );
    await weeks.first().click();
    await expect(page).toHaveURL(/day=week-/);
    await period.getByRole("link", { name: "30 days", exact: true }).click();
    await expect(chart.locator('[data-granularity="day"] button')).toHaveCount(
      30,
    );
    await expect(page).not.toHaveURL(/day=/);
    expect(calls).toBe(0);
    // Keyboard activation also opens the full bounded list, including runs without commits.
    const day = chart
      .getByRole("button", { name: /, [1-9]\d* commits/ })
      .last();
    await day.focus();
    await page.keyboard.press("Enter");
    await expect(page).toHaveURL(/day=\d{4}-\d{2}-\d{2}/);
    await expect(list).toContainText("Grouped push");
    const bounds = await list.evaluate((el) => ({
      overflow: getComputedStyle(el).overflowY,
      max: getComputedStyle(el).maxHeight,
    }));
    expect(bounds.overflow).toBe("auto");
    expect(bounds.max).not.toBe("none");
    expect(calls).toBeLessThanOrEqual(path === "/" ? 0 : 2); // repository selection loads its commit context
    if (path !== "/") {
      await expect(page).toHaveURL(/ref=feature/);
      await expect(page).toHaveURL(/history=1/);
    }
  });
}
