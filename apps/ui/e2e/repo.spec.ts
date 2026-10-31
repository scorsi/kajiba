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
  await expect(refs.locator("option")).toHaveText([
    "feature",
    "main",
    "feature",
    "v1",
  ]);

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

test("alice/demo: history per branch, filtered by a file, a commit and its diff", async ({
  page,
}) => {
  await authenticator(page);
  await enroll(page, enrollLink());
  await page.getByRole("link", { name: "Continue to kajiba" }).click();

  // main has one commit, feature two.
  await page.goto("/alice/demo?history=1");
  const history = page.getByRole("region", { name: "History" });
  await expect(history.getByText("Add the README and a guide")).toBeVisible();
  await expect(history.getByText("Extend the guide")).toHaveCount(0);
  await page.goto("/alice/demo?history=1&ref=feature");
  await expect(history.getByText("Extend the guide")).toBeVisible();

  // Filtered by the nested file: both commits touch it, the README's is not asked.
  await page.goto(
    "/alice/demo?history=1&ref=feature&path=docs%2Fguide%2Fintro.md",
  );
  await expect(page.getByText("2 commits touch")).toBeVisible();
  await page.goto("/alice/demo?history=1&ref=feature&path=README.md");
  await expect(page.getByText("1 commits touch")).toBeVisible();

  // A commit, and the files it changed.
  await page.goto("/alice/demo?history=1&ref=feature");
  await history.getByRole("link", { name: /Extend the guide/ }).click();
  await expect(page).toHaveURL(/at=[0-9a-f]{40}/);
  await expect(
    page.getByRole("heading", { name: "Extend the guide" }),
  ).toBeVisible();
  await expect(page.getByText("1 files changed")).toBeVisible();
  await expect(page.getByText("As of")).toBeVisible();
  await page.getByRole("link", { name: "Back to now" }).click();
  await expect(page).not.toHaveURL(/at=/);
});

test("alice/demo: switching ref and path in place shows the new one at once", async ({
  page,
}) => {
  await authenticator(page);
  await enroll(page, enrollLink());
  await page.getByRole("link", { name: "Continue to kajiba" }).click();

  // Same route, other query: History follows the ref chosen in the switcher.
  await page.goto("/alice/demo?history=1");
  const history = page.getByRole("region", { name: "History" });
  await expect(history.getByText("Add the README and a guide")).toBeVisible();
  await expect(history.getByText("Extend the guide")).toHaveCount(0);
  await page.getByLabel("Branch or tag").selectOption("refs/heads/feature");
  await expect(page).toHaveURL(/ref=feature/);
  await expect(history.getByText("Extend the guide")).toBeVisible();
  await page.getByLabel("Branch or tag").selectOption("refs/heads/main");
  await expect(history.getByText("Extend the guide")).toHaveCount(0);

  // Files: another file in the same column replaces the preview.
  await page.goto("/alice/demo?files=1&path=docs%2Fguide%2Fintro.md");
  await expect(page.getByText("Start here.")).toBeVisible();
  await page.goto("/alice/demo?files=1");
  await page
    .getByRole("region", { name: "root" })
    .getByRole("link", { name: /README\.md/ })
    .click();
  await expect(page).toHaveURL(/path=README\.md/);
  await expect(page.getByText("Start here.")).toHaveCount(0);
  await page
    .getByRole("region", { name: "root" })
    .getByRole("link", { name: /^docs/ })
    .click();
  await page
    .getByRole("region", { name: "docs" })
    .getByRole("link", { name: /^guide/ })
    .click();
  await page
    .getByRole("region", { name: "docs/guide" })
    .getByRole("link", { name: /intro\.md/ })
    .click();
  await expect(page.getByText("Start here.")).toBeVisible();
});
