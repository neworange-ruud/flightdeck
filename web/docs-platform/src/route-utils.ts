import path from "node:path";

import type { DocsPage } from "./types";

export function toRoute(slugSegments: string[], mountPath: string) {
  const suffix = slugSegments.length === 0 ? "" : `/${slugSegments.join("/")}`;

  return `${mountPath === "/" ? "" : mountPath}${suffix}` || "/";
}

export function resolveDocHref(
  href: string,
  page: Pick<DocsPage, "isIndex" | "slugSegments">,
  mountPath: string,
) {
  if (href.startsWith("#") || /^(?:[a-z]+:)?\/\//i.test(href)) {
    return href;
  }

  const [pathnameWithQuery = "", hash = ""] = href.split("#");
  const [pathname = "", query = ""] = pathnameWithQuery.split("?");
  const baseSegments = pathname.startsWith("/")
    ? []
    : page.isIndex
      ? page.slugSegments
      : page.slugSegments.slice(0, -1);
  const normalized = path.posix
    .join("/", ...baseSegments, pathname || "./")
    .replace(/\.mdx?$/i, "")
    .replace(/\/index$/i, "")
    .replace(/\/+/g, "/");
  const route = toRoute(
    normalized.split("/").filter(Boolean),
    mountPath,
  );

  return `${route}${query ? `?${query}` : ""}${hash ? `#${hash}` : ""}`;
}
