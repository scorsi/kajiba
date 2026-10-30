/** View helpers shared by repository pages (URLs, labels, icons). */

import { EntryKind, Role, type Repo } from "./api";
import type { IconName } from "./components/Icon.svelte";
import { refShortName } from "./format";

export const ENTRY_ICON: Record<EntryKind, IconName> = {
  [EntryKind.UNSPECIFIED]: "file",
  [EntryKind.BLOB]: "file",
  [EntryKind.TREE]: "folder",
  [EntryKind.SYMLINK]: "symlink",
  [EntryKind.SUBMODULE]: "submodule",
};

/** `git ls-tree`-style short labels. */
export const ENTRY_LABEL: Record<EntryKind, string> = {
  [EntryKind.UNSPECIFIED]: "?",
  [EntryKind.BLOB]: "blob",
  [EntryKind.TREE]: "tree",
  [EntryKind.SYMLINK]: "link",
  [EntryKind.SUBMODULE]: "module",
};

export const ROLE_LABEL: Record<Role, string> = {
  [Role.UNSPECIFIED]: "—",
  [Role.READ]: "read",
  [Role.WRITE]: "write",
  [Role.ADMIN]: "admin",
};

export const repoPath = (r: Pick<Repo, "owner" | "name">) =>
  `/${r.owner}/${r.name}`;

/** `/<owner>/<name>/tree/<ref>/<path>` — `ref` is shown short (`main`, `v1.0`). */
export function treeHref(
  r: Pick<Repo, "owner" | "name">,
  ref: string,
  path = "",
): string {
  const p = path.split("/").filter(Boolean).map(encodeURIComponent).join("/");
  return `${repoPath(r)}/tree/${encodeURI(refShortName(ref))}${p ? "/" + p : ""}`;
}

export function commitsHref(
  r: Pick<Repo, "owner" | "name">,
  ref: string,
): string {
  return `${repoPath(r)}/commits/${encodeURI(refShortName(ref))}`;
}

/** Commit page: a commit id is a valid ref for `/commits/<ref>`; ASSUMED `/commit/<id>`. */
export function commitHref(
  r: Pick<Repo, "owner" | "name">,
  id: string,
): string {
  return `${repoPath(r)}/commit/${id}`;
}

/**
 * Split `<ref>/<path>` from a URL where the ref itself may contain slashes
 * (`feat/runner-labels/src/main.rs`): the longest known branch, then tag, wins;
 * a full commit id is accepted too. Returns the full ref name or the id.
 */
export function splitRefPath(
  rest: string,
  refs: { name: string }[],
): { revision: string; path: string } | undefined {
  const segs = rest.split("/").filter(Boolean); // route params arrive decoded
  for (let i = segs.length; i >= 1; i--) {
    const short = segs.slice(0, i).join("/");
    for (const prefix of ["refs/heads/", "refs/tags/"]) {
      if (refs.some((r) => r.name === prefix + short)) {
        return { revision: prefix + short, path: segs.slice(i).join("/") };
      }
    }
  }
  if (segs[0] && /^[0-9a-f]{40}$/.test(segs[0])) {
    return { revision: segs[0], path: segs.slice(1).join("/") };
  }
  return undefined;
}
