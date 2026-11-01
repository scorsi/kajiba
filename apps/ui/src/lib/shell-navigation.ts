/** Run pages belong to Activity, which contains the forge-wide Pipelines band. */
export function isCurrentPage(pathname: string, href: string): boolean {
  if (href === "/") return pathname === "/" || pathname.startsWith("/-/runs/");
  return pathname === href || pathname.startsWith(href + "/");
}
