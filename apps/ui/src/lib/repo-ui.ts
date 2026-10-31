/** Labels and icons shared by views. */

import { EntryKind, Role } from "./api";
import type { IconName } from "./components/Icon.svelte";

export const ENTRY_ICON: Record<EntryKind, IconName> = {
  [EntryKind.UNSPECIFIED]: "file",
  [EntryKind.BLOB]: "file",
  [EntryKind.EXECUTABLE]: "file",
  [EntryKind.TREE]: "folder",
  [EntryKind.LINK]: "symlink",
  [EntryKind.SUBMODULE]: "submodule",
};

export const ROLE_LABEL: Record<Role, string> = {
  [Role.UNSPECIFIED]: "—",
  [Role.READ]: "read",
  [Role.WRITE]: "write",
  [Role.ADMIN]: "admin",
};
