import Link from "next/link";
import type { ReactNode } from "react";

import type { DocsNavigationSection, DocsSiteConfig } from "../types";

export function DocsLayout({ children, currentRoute, navigationSections, siteConfig }: { children: ReactNode; currentRoute: string; navigationSections: DocsNavigationSection[]; siteConfig: DocsSiteConfig }) {
  return (
    <div className="min-h-screen bg-[color:var(--docs-bg)] text-[color:var(--docs-fg)]">
      <header className="sticky top-0 z-10 border-b border-[color:var(--docs-border)] bg-[color:var(--docs-bg)]/90 backdrop-blur">
        <div className="mx-auto flex max-w-7xl items-center justify-between px-5 py-4">
          <Link className="font-semibold tracking-tight" href={siteConfig.mountPath}>{siteConfig.siteTitle}</Link>
          {siteConfig.repositoryUrl ? <a className="text-sm text-[color:var(--docs-muted)] hover:text-[color:var(--docs-fg)]" href={siteConfig.repositoryUrl} rel="noreferrer" target="_blank">GitHub</a> : null}
        </div>
      </header>
      <div className="mx-auto grid max-w-7xl gap-8 px-5 py-8 lg:grid-cols-[15rem_minmax(0,1fr)]">
        <aside className="lg:sticky lg:top-20 lg:h-fit">
          <nav aria-label="Documentation navigation" className="space-y-6">
            {navigationSections.map((group, index) => (
              <div key={group.section ?? `group-${index}`}>
                {group.section ? <p className="border-t px-3 pb-1.5 pt-2.5 text-sm font-semibold uppercase tracking-wider text-[color:var(--docs-accent)]">{group.section}</p> : null}
                <ul className="space-y-1">
                  {group.items.map((item) => <li key={item.route}><Link className={`block rounded-lg px-3 py-2 text-sm transition ${item.route === currentRoute ? "bg-[color:var(--docs-surface)] font-semibold" : "text-[color:var(--docs-muted)] hover:bg-[color:var(--docs-surface)] hover:text-[color:var(--docs-fg)]"}`} href={item.route}>{item.label}</Link></li>)}
                </ul>
              </div>
            ))}
          </nav>
        </aside>
        <main className="docs-prose min-w-0 max-w-3xl">{children}</main>
      </div>
    </div>
  );
}
