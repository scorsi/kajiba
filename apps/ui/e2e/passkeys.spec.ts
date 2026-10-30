// Passkey enrollment, login, logout and session expiry against both daemons (0014, 0028,
// 0079, 0084, 0085), with Chromium's WebAuthn virtual authenticator.
import { execFileSync } from "node:child_process";
import { expect, test, type Page } from "@playwright/test";

/** A new link for alice, as printed by `kajiba-server admin enroll` (0028). */
function enrollLink(): string {
  const socket = process.env.KAJIBA_E2E_ADMIN_SOCKET;
  if (!socket)
    throw new Error(
      "KAJIBA_E2E_ADMIN_SOCKET is unset: run through nix run .#e2e",
    );
  const out = execFileSync(
    "kajiba-server",
    ["admin", "--socket", socket, "enroll", "alice"],
    {
      encoding: "utf8",
    },
  );
  const link = out.trim();
  expect(link).toMatch(/^http:\/\/localhost:\d+\/-\/enroll#[0-9a-f]{64}$/);
  return link;
}

/** A platform authenticator holding discoverable credentials, verifying the user at once. */
async function authenticator(page: Page) {
  const cdp = await page.context().newCDPSession(page);
  await cdp.send("WebAuthn.enable");
  await cdp.send("WebAuthn.addVirtualAuthenticator", {
    options: {
      protocol: "ctap2",
      transport: "internal",
      hasResidentKey: true,
      hasUserVerification: true,
      isUserVerified: true,
      automaticPresenceSimulation: true,
    },
  });
}

async function enroll(page: Page, link: string) {
  // From another document: a link differing only by its fragment would not reload the page.
  await page.goto("about:blank");
  await page.goto(link);
  await expect(
    page.getByRole("heading", { name: "Welcome, alice" }),
  ).toBeVisible();
  await expect(page).toHaveURL(/\/-\/enroll$/);
  await page.getByLabel("Passkey name").fill("virtual");
  await page.getByRole("button", { name: "Create passkey" }).click();
  await expect(
    page.getByRole("heading", { name: "Passkey registered" }),
  ).toBeVisible();
}

async function signIn(page: Page) {
  await page.goto("/-/login");
  await page.getByRole("button", { name: "Sign in with a passkey" }).click();
}

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
