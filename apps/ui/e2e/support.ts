// Helpers shared by the e2e scenarios: enrollment links, the virtual authenticator, sign-in.
import { execFileSync } from "node:child_process";
import { expect, type Page } from "@playwright/test";

/** A new link for alice, as printed by `kajiba-server admin enroll` (0028). */
export function enrollLink(): string {
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
export async function authenticator(page: Page) {
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

export async function enroll(page: Page, link: string) {
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

export async function signIn(page: Page) {
  await page.goto("/-/login");
  await page.getByRole("button", { name: "Sign in with a passkey" }).click();
}
