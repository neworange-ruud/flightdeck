import Link from "next/link";
import type { ReactNode } from "react";

import type { DocsNavigationItem, DocsNavigationSection, DocsSiteConfig } from "../types";

function NavItems({ currentRoute, items }: { currentRoute: string; items: DocsNavigationItem[] }) {
  return (
    <ul className="space-y-1">
      {items.map((item) => <li key={item.route}><Link className={`block rounded-lg px-3 py-2 text-sm transition ${item.route === currentRoute ? "bg-[color:var(--docs-surface)] font-semibold" : "text-[color:var(--docs-muted)] hover:bg-[color:var(--docs-surface)] hover:text-[color:var(--docs-fg)]"}`} href={item.route}>{item.label}</Link></li>)}
    </ul>
  );
}

export function DocsLayout({ children, currentRoute, navigationSections, siteConfig }: { children: ReactNode; currentRoute: string; navigationSections: DocsNavigationSection[]; siteConfig: DocsSiteConfig }) {
  const { collapsible, collapsedSections } = siteConfig.navigation;
  const isCollapsed = (group: DocsNavigationSection) =>
    collapsible && group.section !== undefined && collapsedSections.includes(group.section);
  const primary = navigationSections.filter((group) => !isCollapsed(group));
  const folded = navigationSections.filter(isCollapsed);

  return (
    <div className="min-h-screen bg-[color:var(--docs-bg)] text-[color:var(--docs-fg)]">
      <header className="sticky top-0 z-10 border-b border-[color:var(--docs-border)] bg-[color:var(--docs-bg)]/90 backdrop-blur">
        <div className="mx-auto flex max-w-7xl items-center justify-between px-5 py-4">
          <Link className="font-semibold tracking-tight" href={siteConfig.mountPath}>{siteConfig.siteTitle}</Link>
          {siteConfig.repositoryUrl ? <a className="text-sm text-[color:var(--docs-muted)] hover:text-[color:var(--docs-fg)]" href={siteConfig.repositoryUrl} rel="noreferrer" target="_blank">GitHub</a> : null}
        </div>
      </header>
      <div className="mx-auto grid max-w-7xl gap-8 px-5 py-8 lg:grid-cols-[15rem_minmax(0,1fr)]">
        <aside className="lg:sticky lg:top-20 lg:max-h-[calc(100vh-6rem)] lg:overflow-y-auto">
          <nav aria-label="Documentation navigation" className="space-y-6">
            {primary.map((group, index) => (
              <div key={group.section ?? `group-${index}`}>
                {group.section ? <p className="border-t px-3 pb-1.5 pt-2.5 text-sm font-semibold uppercase tracking-wider text-[color:var(--docs-accent)]">{group.section}</p> : null}
                <NavItems currentRoute={currentRoute} items={group.items} />
              </div>
            ))}
            {folded.length > 0 ? (
              <div className="border-t pt-2.5">
                <p className="px-3 pb-1 text-xs font-semibold uppercase tracking-wider text-[color:var(--docs-muted)]">Advanced</p>
                {folded.map((group) => (
                  <details className="docs-nav-fold" key={group.section} open={group.items.some((item) => item.route === currentRoute)}>
                    <summary className="cursor-pointer rounded-lg px-3 py-2 text-sm font-medium text-[color:var(--docs-muted)] hover:bg-[color:var(--docs-surface)] hover:text-[color:var(--docs-fg)]">{group.section}</summary>
                    <div className="pl-3">
                      <NavItems currentRoute={currentRoute} items={group.items} />
                    </div>
                  </details>
                ))}
              </div>
            ) : null}
          </nav>
        </aside>
        <main className="docs-prose min-w-0 max-w-3xl">{children}</main>
      </div>
    </div>
  );
}
