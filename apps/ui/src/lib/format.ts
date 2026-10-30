/** Pure view helpers: no data access, no DOM. */

import type { Timestamp } from "./api";

export const shortId = (id: string) => id.slice(0, 7);

/** git's null object id (added / deleted side of a change). */
export const ZERO_ID = "0".repeat(40);

/** `refs/heads/main` → `main`, `refs/tags/v1` → `v1`. */
export function refShortName(ref: string): string {
  return ref.replace(/^refs\/(heads|tags)\//, "");
}

export function refKind(ref: string): "branch" | "tag" | "other" {
  if (ref.startsWith("refs/heads/")) return "branch";
  if (ref.startsWith("refs/tags/")) return "tag";
  return "other";
}

export function toDate(ts: Timestamp | undefined): Date | undefined {
  return ts
    ? new Date(Number(ts.seconds) * 1000 + Math.floor(ts.nanos / 1e6))
    : undefined;
}

const rtf = new Intl.RelativeTimeFormat("en", { numeric: "auto" });
const UNITS: [Intl.RelativeTimeFormatUnit, number][] = [
  ["year", 31536000],
  ["month", 2592000],
  ["week", 604800],
  ["day", 86400],
  ["hour", 3600],
  ["minute", 60],
];

export function relativeTime(date: Date | undefined, now = Date.now()): string {
  if (!date) return "";
  const delta = (date.getTime() - now) / 1000;
  for (const [unit, secs] of UNITS) {
    if (Math.abs(delta) >= secs)
      return rtf.format(Math.round(delta / secs), unit);
  }
  return "just now";
}

/** ISO-ish absolute date for `title` / `<time datetime>`. */
export function absoluteTime(date: Date | undefined): string {
  return date ? date.toISOString().replace("T", " ").slice(0, 16) + " UTC" : "";
}

export const commitSubject = (message: string) => message.split("\n", 1)[0];
export const commitBody = (message: string) =>
  message.split("\n").slice(1).join("\n").trim();

export function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 ** 2) return `${(n / 1024).toFixed(1)} KiB`;
  if (n < 1024 ** 3) return `${(n / 1024 ** 2).toFixed(1)} MiB`;
  return `${(n / 1024 ** 3).toFixed(2)} GiB`;
}

/** Heuristic used by git itself: a NUL byte in the first 8 KiB means binary. */
export function isBinary(bytes: Uint8Array): boolean {
  return bytes.subarray(0, 8000).includes(0);
}

export interface CloneUrls {
  https: string;
  ssh: string;
}

/** Clone URLs from the current origin (ASSUMED: SSH user/port fixed by ADR). */
export function cloneUrls(
  owner: string,
  name: string,
  host = location.host,
): CloneUrls {
  const hostname = host.replace(/:\d+$/, "");
  return {
    https: `https://${host}/${owner}/${name}.git`,
    ssh: `ssh://kajiba-git@${hostname}:2222/${owner}/${name}.git`,
  };
}

export const README_NAMES = [
  "README.md",
  "readme.md",
  "README.markdown",
  "README",
  "README.txt",
];
