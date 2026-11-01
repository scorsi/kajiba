import type { Run } from "#lib/api.ts";
import { refKind, refShortName, shortId } from "#lib/format.ts";

export function runLabel(run: Run): string {
  if (run.trigger !== "worktree") {
    const kind = refKind(run.ref);
    return kind === "other" ? run.ref : `${kind}: ${refShortName(run.ref)}`;
  }
  const base = run.baseCommitId ? shortId(run.baseCommitId) : "unknown base";
  const checks = run.checks.length ? ` · checks: ${run.checks.join(", ")}` : "";
  return `worktree by ${run.triggeredBy || "unknown user"} on ${base}${checks}`;
}
