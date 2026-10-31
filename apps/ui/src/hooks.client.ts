import type { HandleClientError } from "@sveltejs/kit/hooks";
import { describeError } from "#lib/errors.ts";

// Keep what the error page needs: SvelteKit would otherwise show "Internal Error".
export const handleError: HandleClientError = ({ error, event }) => {
  const r = describeError(error, event.url.pathname + event.url.search);
  return { message: r.message, report: r };
};
