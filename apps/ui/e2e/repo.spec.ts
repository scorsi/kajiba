// The repository pages on the real API (0013, 0021, 0026, 0054): the shell lists the
// declared repository, its README renders, refs, clone URLs and declared access show.
import { expect, test } from "@playwright/test";
import { authenticator, enroll, enrollLink } from "./support";

test("alice/demo: README, refs, clone URLs and read-only declared access", async ({
  page,
}) => {
  await authenticator(page);
  await enroll(page, enrollLink());
  await page.getByRole("link", { name: "Continue to kajiba" }).click();

  const link = page.locator('a[href="/alice/demo"]').first();
  await expect(link).toBeVisible();
  await link.click();
  await expect(page).toHaveURL(/\/alice\/demo$/);
  await expect(
    page.getByRole("heading", { name: "alice / demo" }),
  ).toBeVisible();
  await expect(page.getByText("A fixture repository.")).toBeVisible();

  // The ref switcher lists both branches and the tag.
  const refs = page.getByLabel("Branch or tag");
  await expect(refs.locator("option")).toHaveText(["feature", "main", "v1"]);

  // Clone shows the server's URLs, not the page's origin.
  await page.locator("summary", { hasText: "Clone" }).click();
  await expect(page.getByLabel("HTTPS clone URL")).toHaveValue(
    /^http:\/\/localhost:\d+\/alice\/demo\.git$/,
  );
  await expect(page.getByLabel("SSH clone URL")).toHaveValue(
    /^ssh:\/\/.+@localhost:2222\/alice\/demo\.git$/,
  );

  // Declared permissions show, with no edit control.
  await expect(page.getByText("reader read")).toBeVisible();
  await expect(page.getByText("declared")).toBeVisible();
  await expect(
    page.getByRole("button", { name: /edit|remove|add/i }),
  ).toHaveCount(0);
});

test("alice/demo: open a nested file from the Files band, its size and raw bytes", async ({
  page,
}) => {
  await authenticator(page);
  await enroll(page, enrollLink());
  await page.getByRole("link", { name: "Continue to kajiba" }).click();
  await page.goto("/alice/demo?files=1");

  // Descend from the root column: docs, guide, intro.md.
  const column = (name: string) => page.getByRole("region", { name });
  await column("root").getByRole("link", { name: /^docs/ }).click();
  await column("docs")
    .getByRole("link", { name: /^guide/ })
    .click();
  const intro = column("docs/guide").getByRole("link", { name: /intro\.md/ });
  await expect(intro).toContainText("12 B");
  await intro.click();
  await expect(page).toHaveURL(/path=docs%2Fguide%2Fintro\.md/);
  await expect(page.getByText("Start here.")).toBeVisible();

  // The raw link answers the same bytes.
  const raw = page.getByRole("link", { name: "Raw" });
  await expect(raw).toHaveAttribute(
    "href",
    "/raw/alice/demo/main/docs/guide/intro.md",
  );
  const response = await page.request.get(
    "/raw/alice/demo/main/docs/guide/intro.md",
  );
  expect(await response.text()).toBe("Start here.\n");
});
