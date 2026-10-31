// Passkey enrollment, login, logout and session expiry against both daemons (0014, 0028,
// 0079, 0084, 0085), the account page's passkeys, tokens and declared credentials (0081,
// 0143), with Chromium's WebAuthn virtual authenticator.
import { expect, test, type Page } from "@playwright/test";
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

test("account: add a passkey, sign in with it, remove the first; tokens; declared credentials", async ({
  page,
}) => {
  const { cdp, authenticatorId } = await authenticator(page);
  await enroll(page, enrollLink());
  await page.goto("/-/account?declared=1&keys=1");
  const passkeys = page.getByRole("region", { name: "Passkeys" });
  await expect(passkeys.getByText("virtual")).toBeVisible();
  // The only passkey cannot be removed.
  await expect(passkeys.getByRole("button", { name: "Remove" })).toBeDisabled();

  // Confirm with the existing passkey, then create the second one on another
  // authenticator: the first one is excluded, and holding only the second, the new
  // authenticator signs in with it alone.
  await passkeys.getByLabel("Name of the new passkey").fill("second");
  await passkeys.getByRole("button", { name: "Add a passkey" }).click();
  const create = passkeys.getByRole("button", {
    name: "Create the new passkey",
  });
  await expect(create).toBeVisible();
  await cdp.send("WebAuthn.removeVirtualAuthenticator", { authenticatorId });
  await authenticator(page);
  await create.click();
  await expect(passkeys.getByText("second")).toBeVisible();
  await expect(passkeys.getByRole("listitem")).toHaveCount(2);

  await page.getByRole("button", { name: "Sign out" }).click();
  await signIn(page);
  await signedIn(page);

  // Remove the first passkey, confirmed with the second.
  await page.goto("/-/account?declared=1&keys=1");
  const virtual = passkeys.getByRole("listitem").filter({ hasText: "virtual" });
  await virtual.getByRole("button", { name: "Remove" }).click();
  await virtual.getByRole("button", { name: "Confirm and remove" }).click();
  await expect(passkeys.getByRole("listitem")).toHaveCount(1);
  await expect(passkeys.getByText("second")).toBeVisible();
  await expect(passkeys.getByRole("button", { name: "Remove" })).toBeDisabled();

  // A token: created, shown once, revoked.
  const tokens = page.getByRole("region", { name: "Personal tokens" });
  await tokens.getByRole("button", { name: "New token" }).click();
  await tokens.getByLabel("Name").fill("e2e");
  await tokens.getByRole("button", { name: "Create token" }).click();
  await expect(tokens.getByLabel("New token secret")).toHaveValue(/^kjb_/);
  const token = tokens.getByRole("listitem").filter({ hasText: "e2e" });
  await token.getByRole("button", { name: "Revoke" }).click();
  await token.getByRole("button", { name: "Revoke" }).click();
  await expect(token.getByText("revoked")).toBeVisible();

  // Declared token and keys: shown with a lock, no control.
  for (const name of ["Declared tokens", "Declared keys"]) {
    const region = page.getByRole("region", { name });
    await expect(region.getByRole("listitem").first()).toBeVisible();
    await expect(region.getByRole("button")).toHaveCount(0);
  }
  const declared = page.getByRole("region", { name: "Declared tokens" });
  await expect(declared.getByText("ci", { exact: true })).toBeVisible();
  const keys = page.getByRole("region", { name: "Declared keys" });
  await expect(keys.getByText("Authentication")).toBeVisible();
  await expect(keys.getByText("Signing")).toBeVisible();
  await expect(keys.getByText(/ssh-ed25519 SHA256:/)).toHaveCount(2);
});
