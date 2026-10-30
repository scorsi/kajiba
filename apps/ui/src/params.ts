import { defineParams } from "@sveltejs/kit/params";

/** Route param matchers (`[owner=slug]`). */
export const params = defineParams({
  /** Owner and repository names: `[a-z0-9][a-z0-9._-]{0,62}` (ADR). */
  slug: (param) =>
    /^[a-z0-9][a-z0-9._-]{0,62}$/.test(param) ? param : undefined,
});
