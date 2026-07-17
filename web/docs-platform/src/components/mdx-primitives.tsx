import { compileMDX } from "next-mdx-remote/rsc";
import Link from "next/link";
import rehypeAutolinkHeadings from "rehype-autolink-headings";
import rehypeSlug from "rehype-slug";
import remarkGfm from "remark-gfm";
import type { AnchorHTMLAttributes, ReactNode } from "react";

import { resolveDocHref } from "../route-utils";
import type { DocsPage, DocsSiteConfig } from "../types";

const linkClass = "font-medium text-[color:var(--docs-accent)] underline decoration-[color:color-mix(in_srgb,var(--docs-accent)_35%,transparent)] underline-offset-4";

export function Callout({ children, type = "info" }: { children: ReactNode; type?: "info" | "note" | "success" | "warning" }) {
  return <aside className="my-8 rounded-2xl border border-[color:var(--docs-border)] bg-[color:var(--docs-surface)] px-5 py-4 shadow-sm"><strong className="block text-sm uppercase tracking-wider text-[color:var(--docs-accent)]">{type}</strong><div className="mt-2">{children}</div></aside>;
}

export function CardGrid({ children }: { children: ReactNode }) {
  return <div className="my-8 grid gap-4 md:grid-cols-2">{children}</div>;
}

export function Badge({ children, tone = "neutral" }: { children: ReactNode; tone?: "info" | "neutral" | "success" | "warning" }) {
  return <span className={`inline-flex rounded-full border px-2.5 py-1 text-xs font-semibold uppercase ${tone === "success" ? "border-emerald-400 text-emerald-600" : "border-[color:var(--docs-border)] text-[color:var(--docs-muted)]"}`}>{children}</span>;
}

function DocsLink({ currentPage, siteConfig, ...props }: AnchorHTMLAttributes<HTMLAnchorElement> & { currentPage: DocsPage; siteConfig: DocsSiteConfig }) {
  const href = props.href ?? "#";
  if (href.startsWith("#") || /^(?:[a-z]+:)?\/\//i.test(href)) return <a {...props} className={linkClass} href={href} />;
  return <Link className={linkClass} href={resolveDocHref(href, currentPage, siteConfig.mountPath)}>{props.children}</Link>;
}

function Card({ children, href, title, currentPage, siteConfig }: { children: ReactNode; href: string; title: string; currentPage: DocsPage; siteConfig: DocsSiteConfig }) {
  const target = /^(?:[a-z]+:)?\/\//i.test(href) ? href : resolveDocHref(href, currentPage, siteConfig.mountPath);
  return <Link className="block rounded-2xl border border-[color:var(--docs-border)] bg-[color:var(--docs-surface)] p-5 transition hover:border-[color:var(--docs-accent)] hover:shadow-md" href={target}><strong className="block text-lg">{title}</strong><span className="mt-2 block text-sm leading-6 text-[color:var(--docs-muted)]">{children}</span></Link>;
}

export async function renderDocsMdx(page: DocsPage, siteConfig: DocsSiteConfig) {
  const { content } = await compileMDX({
    source: page.body,
    components: {
      a: (props) => <DocsLink {...props} currentPage={page} siteConfig={siteConfig} />,
      pre: (props) => <pre className="my-8 overflow-x-auto rounded-2xl bg-slate-950 p-5 text-sm text-slate-100">{props.children}</pre>,
      Badge,
      Callout,
      Card: (props) => <Card {...props} currentPage={page} siteConfig={siteConfig} />,
      CardGrid,
    },
    options: { parseFrontmatter: false, mdxOptions: { remarkPlugins: [remarkGfm], rehypePlugins: [rehypeSlug, [rehypeAutolinkHeadings, { behavior: "wrap" }]] } },
  });
  return content;
}
