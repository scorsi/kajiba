/** Typed contexts shared by layouts and pages. */

import { createContext } from "svelte";
import type { Ref, Repo } from "./api";

/** Current repository, provided by `[owner]/[name]/+layout.svelte`. */
export interface RepoContext {
  readonly repo: Repo;
  readonly refs: Ref[];
  /** `"<owner>/<name>"`, the request key for RepoService calls. */
  readonly key: string;
}

export const [getRepoContext, setRepoContext] = createContext<RepoContext>();
