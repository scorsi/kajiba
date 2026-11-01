/** Pipelines is Activity's band, with its own shell entry for run pages. */
export function isCurrentPage(pathname: string, href: string): boolean {
  if (href === "/?pipelines=1#pipelines")
    return pathname.startsWith("/-/runs/");
  if (href === "/") return pathname === "/";
  return pathname === href || pathname.startsWith(href + "/");
}
