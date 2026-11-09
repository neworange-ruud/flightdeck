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

/**
 * A video in a docs page. Self-hosted (`src`, a file under `web/public/videos/`
 * or any https URL) or a YouTube embed (`youtube`, the video id), never both.
 * `title` is required: it labels the player for screen readers and is the
 * iframe title. Bad props fail the build rather than ship a broken player.
 *
 * ```mdx
 * <Video src="/videos/worker-setup.mp4" poster="/videos/worker-setup.jpg"
 *   captions="/videos/worker-setup.vtt" title="Set up a worker"
 *   caption="Five minutes from SSH to a running agent." />
 * <Video youtube="dQw4w9WgXcQ" title="FlightDeck in 90 seconds" />
 * <Video src="/videos/mission-control.mp4" title="Approve from Mission control" loop />
 * ```
 *
 * `loop` makes a short silent clip that plays like an animated screenshot
 * (muted, autoplay, looping, no controls until hovered by the browser's own UI).
 */
export function Video({ src, webm, poster, captions, youtube, title, caption, loop = false }: { src?: string; webm?: string; poster?: string; captions?: string; youtube?: string; title: string; caption?: string; loop?: boolean }) {
  if (!title?.trim()) throw new Error("<Video> needs a title.");
  if (Boolean(src) === Boolean(youtube)) throw new Error(`<Video title="${title}"> needs exactly one of src or youtube.`);
  if (youtube && !/^[\w-]{11}$/.test(youtube)) throw new Error(`<Video title="${title}">: youtube must be an 11-character video id, not a URL.`);
  if (youtube && (webm || poster || captions || loop)) throw new Error(`<Video title="${title}">: webm, poster, captions and loop only apply to a self-hosted src.`);

  const player = youtube
    ? <iframe allow="encrypted-media; fullscreen; picture-in-picture" allowFullScreen className="aspect-video h-auto w-full" loading="lazy" referrerPolicy="strict-origin-when-cross-origin" src={`https://www.youtube-nocookie.com/embed/${youtube}?rel=0`} title={title} />
    : <video aria-label={title} autoPlay={loop} className="block h-auto w-full" controls={!loop} loop={loop} muted={loop} playsInline poster={poster} preload="metadata">
        {webm ? <source src={webm} type="video/webm" /> : null}
        <source src={src} type="video/mp4" />
        {captions ? <track default kind="captions" label="English" src={captions} srcLang="en" /> : null}
      </video>;

  return (
    <figure className="docs-video my-8">
      <div className="overflow-hidden rounded-[0.9rem] border border-[color:var(--docs-border)] bg-[color:var(--docs-surface)] shadow-[0_12px_34px_-14px_rgba(2,8,20,0.65)]">{player}</div>
      {caption ? <figcaption className="mt-3 text-center text-[0.85rem] text-[color:var(--docs-muted)]">{caption}</figcaption> : null}
    </figure>
  );
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
      Video,
    },
    options: { parseFrontmatter: false, mdxOptions: { remarkPlugins: [remarkGfm], rehypePlugins: [rehypeSlug, [rehypeAutolinkHeadings, { behavior: "wrap" }]] } },
  });
  return content;
}
