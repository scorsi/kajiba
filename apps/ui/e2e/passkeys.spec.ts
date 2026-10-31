// Passkey enrollment, login, logout and session expiry against both daemons (0014, 0028,
// 0079, 0084, 0085), with Chromium's WebAuthn virtual authenticator.
import { expect, test } from "@playwright/test";
import { authenticator, enroll, enrollLink, signIn } from "./support";

const signedIn = (page: Page) =>
  expect(page.getByRole("link", { name: "alice", exact: true })).toBeVisible();
const signedOut = (page: Page) =>
  expect(page.getByRole("heading", { name: "Sign in required" })).toBeVisible();

test("enrollment, logout, login again, and a session refused after its lifetime", async ({
  page,
  context,
}) => {
  test.setTimeout(150_000);
  await authenticator(page);
  await enroll(page, enrollLink());
  await page.getByRole("link", { name: "Continue to kajiba" }).click();
  await signedIn(page);

  await page.getByRole("button", { name: "Sign out" }).click();
  await expect(page).toHaveURL(/\/-\/login$/);
  await page.goto("/");
  await signedOut(page);

  await signIn(page);
  await expect(page).toHaveURL(/\/$/);
  await signedIn(page);

  // auth.session_ttl is 1m: put the cookie back after the browser dropped it, so the
  // server itself has to refuse the expired session.
  const [session] = (await context.cookies()).filter(
    (c) => c.name === "kajiba_session",
  );
  expect(session).toBeDefined();
  await page.waitForTimeout(62_000);
  await context.addCookies([{ ...session, expires: -1 }]);
  await page.goto("/");
  await signedOut(page);
});

test("a second admin enroll revokes the passkey and supersedes the earlier link", async ({
  page,
}) => {
  await authenticator(page);
  await enroll(page, enrollLink());
  await page.getByRole("link", { name: "Continue to kajiba" }).click();
  await page.getByRole("button", { name: "Sign out" }).click();
  await expect(page).toHaveURL(/\/-\/login$/);

  const superseded = enrollLink();
  const current = enrollLink();
  await signIn(page);
  await expect(page.getByRole("alert")).toHaveText(
    "This passkey is not recognised.",
  );

  await page.goto(superseded);
  await expect(
    page.getByRole("heading", { name: "This link can't be used" }),
  ).toBeVisible();

  await enroll(page, current);
});

test("a ceremony from another origin is refused and leaves the link usable", async ({
  page,
}) => {
  await authenticator(page);
  const link = enrollLink();
  // The browser signs for localhost; claim another origin in the client data the server
  // verifies (attestation "none": the client data is not signed, only checked).
  await page.route("**/auth/webauthn/enroll/finish", async (route) => {
    const body = route.request().postDataJSON();
    const client = JSON.parse(
      Buffer.from(
        body.credential.response.clientDataJSON,
        "base64url",
      ).toString(),
    );
    client.origin = "https://forge.example.org";
    body.credential.response.clientDataJSON = Buffer.from(
      JSON.stringify(client),
    ).toString("base64url");
    await route.continue({ postData: JSON.stringify(body) });
  });
  await page.goto(link);
  await page.getByRole("button", { name: "Create passkey" }).click();
  await expect(page.getByRole("alert")).toHaveText(
    "The passkey could not be verified.",
  );

  await page.unroute("**/auth/webauthn/enroll/finish");
  await enroll(page, link);
});
