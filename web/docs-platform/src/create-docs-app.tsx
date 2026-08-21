import { cache } from "react";
import type { Metadata } from "next";
import Link from "next/link";
import { notFound } from "next/navigation";

import { DocsLayout } from "./components/docs-layout";
import { renderDocsMdx } from "./components/mdx-primitives";
import { loadDocsTree } from "./docs-data";
import { toRoute } from "./route-utils";
import type { DocsSiteDefinition } from "./types";

const getSiteData = cache(async (siteDir: string) => loadDocsTree(siteDir));
type RouteParams = Promise<{ slug?: string[] }> | { slug?: string[] };

export function createDocsApp(site: DocsSiteDefinition) {
  async function Page({ params }: { params: RouteParams }) {
    const { slug = [] } = await params;
    const siteData = await getSiteData(site.siteDir);
    const route = toRoute(slug, siteData.siteConfig.mountPath);
    const page = siteData.pagesByRoute.get(route);
    if (!page) notFound();
    const content = await renderDocsMdx(page, siteData.siteConfig);
    // Previous/next follow the sidebar navigation order (grouped by section),
    // not filesystem order, so they match what the reader sees in the nav.
    const navIndex = siteData.navigation.findIndex((item) => item.route === page.route);
    const previousItem = navIndex > 0 ? siteData.navigation[navIndex - 1] : undefined;
    const nextItem = navIndex >= 0 ? siteData.navigation[navIndex + 1] : undefined;
    const previous = previousItem ? siteData.pagesByRoute.get(previousItem.route) : undefined;
    const next = nextItem ? siteData.pagesByRoute.get(nextItem.route) : undefined;

    return <DocsLayout currentRoute={page.route} navigationSections={siteData.navigationSections} siteConfig={siteData.siteConfig}><article>{content}<nav className="mt-12 flex justify-between gap-4 border-t border-[color:var(--docs-border)] pt-6">{previous ? <Link href={previous.route}>Previous: {previous.frontmatter.label ?? previous.title}</Link> : <span />}{next ? <Link href={next.route}>Next: {next.frontmatter.label ?? next.title}</Link> : null}</nav></article></DocsLayout>;
  }

  async function generateStaticParams() {
    const siteData = await getSiteData(site.siteDir);
    return siteData.pages.map((page) => ({ slug: page.slugSegments }));
  }

  async function generateMetadata({ params }: { params: RouteParams }): Promise<Metadata> {
    const { slug = [] } = await params;
    const siteData = await getSiteData(site.siteDir);
    const page = siteData.pagesByRoute.get(toRoute(slug, siteData.siteConfig.mountPath));
    return { title: page ? `${page.title} | ${siteData.siteConfig.siteTitle}` : siteData.siteConfig.siteTitle };
  }

  async function SearchIndexRoute() {
    const siteData = await getSiteData(site.siteDir);
    return Response.json(siteData.pages.filter((page) => !page.frontmatter.hidden).map((page) => ({ route: page.route, title: page.title, text: page.textContent })));
  }

  return { Page, generateMetadata, generateStaticParams, SearchIndexRoute };
}

export function DocsNotFoundPage() {
  return <main className="grid min-h-screen place-items-center bg-[color:var(--docs-bg)] px-6 text-[color:var(--docs-fg)]"><div><p className="text-sm font-semibold tracking-[0.16em] text-[color:var(--docs-muted)] uppercase">404</p><h1 className="mt-3 text-4xl font-semibold">This page does not exist.</h1><Link className="mt-6 inline-block font-semibold text-[color:var(--docs-accent)]" href="/docs">Back to documentation</Link></div></main>;
}
